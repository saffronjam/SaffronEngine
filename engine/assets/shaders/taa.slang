// Temporal anti-aliasing resolve + upsampling. The display-extent spine: dilated (closest-depth)
// reproject → Lanczos-2 reproject-and-accumulate of the input current into the display grid →
// lock-scaled YCoCg variance clip → disocclusion / shading-change gate → reactive, luma-weighted
// velocity/shading-adaptive blend → upscale-aware RCAS sharpen at display extent (display-grid
// reconstructed taps, lobe firming with the upscale ratio). Writes the (optionally sharpened)
// resolved linear-HDR color to the offscreen (what the UI/tonemap reads), the UNSHARPENED resolve +
// accumulated sample count to the next-frame history, and the rolled reconstruction state to the
// next-frame lock. Runs pre-tonemap in one linear-HDR domain. Set 0: 0 current (sampler), 1 history
// (sampler), 2 motion (sampler), 3 offscreen out (storage), 4 history-write out (storage), 5
// motion-prepass depth (sampler, closest-depth dilation), 6 reactive mask (sampler, input extent),
// 7 lock read (sampler, reprojected at histUv), 8 lock write (storage, at tid.xy).
//
// Temporal upsampling: `current`, `motion`, `motionDepth`, and `reactive` are INPUT (render) extent
// while the dispatch, `history`, `lock`, and `offscreen` are DISPLAY extent. The current color is
// reconstructed into the display grid with a Lanczos-2 reproject-and-accumulate resample; the
// variance-clip neighbourhood and the closest-depth dilation step true input texels; history alpha
// carries a per-output accumulated sample count so freshly-covered display pixels lean on the
// reconstructed current and converge over the (ratio-scaled) jitter cycle; FSR2-style pixel locks
// protect stable thin features from the clip; the reactive mask biases alpha-blended pixels toward
// the current frame; and a parallax disocclusion gate (reprojected linear depth from the lock's .b
// channel) forces the current frame whole on gross reveals. Degenerates to the native resolve at
// 1:1 (the Lanczos gather collapses to the exact texel; sampleTarget = the base cycle length).

[[vk::binding(0, 0)]] Sampler2D current;
[[vk::binding(1, 0)]] Sampler2D history;
[[vk::binding(2, 0)]] Sampler2D motion;
[[vk::image_format("rgba16f")]]
[[vk::binding(3, 0)]] RWTexture2D<float4> outColor;
[[vk::image_format("rgba16f")]]
[[vk::binding(4, 0)]] RWTexture2D<float4> outHistory;
[[vk::binding(5, 0)]] Sampler2D motionDepth;
[[vk::binding(6, 0)]] Sampler2D reactive;      // [0..1] reactive mask (input extent)
[[vk::binding(7, 0)]] Sampler2D lockHistory;   // reconstruction state, reprojected at histUv
[[vk::image_format("rgba16f")]]
[[vk::binding(8, 0)]] RWTexture2D<float4> outLock;  // reconstruction state, written at tid.xy

struct Push
{
    float2 feedback;     // x = feedbackMin, y = feedbackMax
    float2 jitter;       // this frame's NDC jitter (carried for the upsampling seam)
    float2 prevJitter;   // last frame's NDC jitter
    float2 screenSize;   // input render extent, px
    float2 gammaValid;   // x = variance-clip gamma, y = 1 if history valid this frame
    float2 rejectSharp;  // x = velocity rejection, y = sharpness (Phase 4)
    float2 upscale;      // x = ratio n = displayW/inputW, y = accumulation target (sampleTarget)
    float2 lockReactive; // x = lock initial lifetime (frames), y = reactive scale
    float2 disoccl;      // x = disocclusion threshold (relative), y = lock-break luma fraction
    float2 depthParams;  // x = camera near, y = camera far (linearize motionDepth)
};
[[vk::push_constant]] Push push;

// perspective_rh_gl device depth (near = min per the motion prepass convention) → positive
// view-space linear depth, for the relative disocclusion comparison.
float LinearizeDepth(float d, float2 nf)
{
    return (nf.x * nf.y) / max(nf.y - d * (nf.y - nf.x), 1e-6);
}

// Lanczos-2 (a = 2) window: sinc(x)·sinc(x/2), zero outside |x| >= 2, 1 at 0. The negative lobes
// reconstruct sharp thin features a bilinear stretch smears.
float Lanczos2(float x)
{
    x = abs(x);
    if (x < 1e-4)
    {
        return 1.0;
    }
    if (x >= 2.0)
    {
        return 0.0;
    }
    const float PI = 3.14159265;
    float px = PI * x;
    return 2.0 * sin(px) * sin(0.5 * px) / (px * px);
}

// FSR2-style resample of the input-extent current color into the display grid: the display
// pixel's position in input-texel space, a 4x4 Lanczos-2 gather. Taps clamp to the valid input
// rect; the result clamps to >= 0 so ringing never injects negative HDR. At 1:1 the covering tap's
// weight is 1 and the rest are 0, so this returns the exact current texel (the native path).
float3 ReconstructCurrent(Sampler2D tex, float2 uv, float2 inputSize)
{
    float2 pos = uv * inputSize;             // sample position, input-texel space
    float2 base = floor(pos - 0.5) + 0.5;    // centre-most texel centre below pos
    float2 inv = 1.0 / inputSize;
    float3 sum = float3(0.0, 0.0, 0.0);
    float wsum = 0.0;
    [unroll]
    for (int j = -1; j <= 2; j = j + 1)
    {
        [unroll]
        for (int i = -1; i <= 2; i = i + 1)
        {
            float2 c = base + float2(i, j);  // this tap's texel centre
            float w = Lanczos2(pos.x - c.x) * Lanczos2(pos.y - c.y);
            float2 tapUv = clamp(c * inv, 0.5 * inv, 1.0 - 0.5 * inv);
            sum += tex.SampleLevel(tapUv, 0.0).rgb * w;
            wsum += w;
        }
    }
    return max(sum / max(wsum, 1e-5), float3(0.0, 0.0, 0.0));
}

// Luma/chroma-decorrelated space for the variance clip: clipping the chroma axes (Co/Cg)
// independently of luma tightens the ellipsoid and removes the purple-fringe a raw-RGB min/max
// box leaves on high-contrast edges.
float3 RGB_to_YCoCg(float3 c)
{
    return float3(
        0.25 * c.r + 0.5 * c.g + 0.25 * c.b,
        0.5 * c.r - 0.5 * c.b,
        -0.25 * c.r + 0.5 * c.g - 0.25 * c.b);
}
float3 YCoCg_to_RGB(float3 c)
{
    return float3(c.x + c.y - c.z, c.x + c.z, c.x - c.y - c.z);
}

// Optimized Catmull-Rom history reconstruction: a 4x4 bicubic footprint gathered with 9
// bilinear taps (Pettineo/TheRealMJP). The negative Catmull-Rom lobes preserve the sharpness a
// single bilinear tap would low-pass away when fed back through history every frame. `texSize`
// is the history extent in pixels (== the current extent today; the upsampling set diverges it).
float3 SampleHistoryCatmullRom(Sampler2D tex, float2 uv, float2 texSize)
{
    float2 samplePos = uv * texSize;
    float2 texPos1 = floor(samplePos - 0.5) + 0.5;
    float2 f = samplePos - texPos1;
    float2 w0 = f * (-0.5 + f * (1.0 - 0.5 * f));
    float2 w1 = 1.0 + f * f * (-2.5 + 1.5 * f);
    float2 w2 = f * (0.5 + f * (2.0 - 1.5 * f));
    float2 w3 = f * f * (-0.5 + 0.5 * f);
    float2 w12 = w1 + w2;
    float2 off12 = w2 / w12;
    float2 p0 = (texPos1 - 1.0) / texSize;
    float2 p3 = (texPos1 + 2.0) / texSize;
    float2 p12 = (texPos1 + off12) / texSize;
    float3 r = float3(0.0, 0.0, 0.0);
    r += tex.SampleLevel(float2(p0.x, p0.y), 0.0).rgb * (w0.x * w0.y);
    r += tex.SampleLevel(float2(p12.x, p0.y), 0.0).rgb * (w12.x * w0.y);
    r += tex.SampleLevel(float2(p3.x, p0.y), 0.0).rgb * (w3.x * w0.y);
    r += tex.SampleLevel(float2(p0.x, p12.y), 0.0).rgb * (w0.x * w12.y);
    r += tex.SampleLevel(float2(p12.x, p12.y), 0.0).rgb * (w12.x * w12.y);
    r += tex.SampleLevel(float2(p3.x, p12.y), 0.0).rgb * (w3.x * w12.y);
    r += tex.SampleLevel(float2(p0.x, p3.y), 0.0).rgb * (w0.x * w3.y);
    r += tex.SampleLevel(float2(p12.x, p3.y), 0.0).rgb * (w12.x * w3.y);
    r += tex.SampleLevel(float2(p3.x, p3.y), 0.0).rgb * (w3.x * w3.y);
    return max(r, float3(0.0, 0.0, 0.0));  // negative-lobe ringing must not push HDR below zero
}

// Clip the history sample toward the neighborhood-statistics box center along the line to the
// history point (Playdead clip_aabb). Tighter and more stable than a hard min/max clamp.
float3 clip_aabb(float3 aMin, float3 aMax, float3 q)
{
    float3 p = 0.5 * (aMax + aMin);
    float3 e = 0.5 * (aMax - aMin) + 1e-5;
    float3 v = q - p;
    float3 a = abs(v / e);
    float m = max(a.x, max(a.y, a.z));
    return m > 1.0 ? p + v / m : q;
}

float Luma(float3 c)
{
    return dot(c, float3(0.2126, 0.7152, 0.0722));
}

// Closest-depth velocity dilation: read the motion vector of the nearest-depth pixel in the 3x3,
// so silhouettes reproject with the foreground surface's velocity (kills edge ghosting). This
// only chooses WHICH stored velocity to read — it never alters velocities, so static geometry
// keeps exact zero motion. The projection is perspective_rh with a LESS depth test, so the near
// plane is the smaller depth: nearest = min.
float2 DilatedMotion(float2 uv, float2 texel, out float closestDepth)
{
    float2 bestOffset = float2(0.0, 0.0);
    float closest = motionDepth.SampleLevel(uv, 0.0).r;
    for (int y = -1; y <= 1; y = y + 1)
    {
        for (int x = -1; x <= 1; x = x + 1)
        {
            float2 o = float2(x, y) * texel;
            float d = motionDepth.SampleLevel(uv + o, 0.0).r;
            if (d < closest)
            {
                closest = d;
                bestOffset = o;
            }
        }
    }
    closestDepth = closest;  // the nearest device depth, for the disocclusion linearization
    return motion.SampleLevel(uv + bestOffset, 0.0).xy;
}

[shader("compute")]
[numthreads(8, 8, 1)]
void computeMain(uint3 tid : SV_DispatchThreadID)
{
    uint width;
    uint height;
    outColor.GetDimensions(width, height);
    if (tid.x >= width || tid.y >= height)
    {
        return;
    }
    float2 texel = 1.0 / float2(width, height);   // DISPLAY texel (output grid)
    float2 uv = (float2(tid.xy) + 0.5) * texel;
    float2 inputTexel = 1.0 / push.screenSize;    // INPUT texel (the source grid to step on)

    // Lanczos-2 reproject-and-accumulate reconstruction of the input current color at this
    // display pixel (degenerates to the exact current texel at 1:1).
    float3 cur = ReconstructCurrent(current, uv, push.screenSize);

    // First + second color moments of the 3x3 current-frame neighborhood, in YCoCg.
    float3 m1 = float3(0.0, 0.0, 0.0);
    float3 m2 = float3(0.0, 0.0, 0.0);
    for (int y = -1; y <= 1; y = y + 1)
    {
        for (int x = -1; x <= 1; x = x + 1)
        {
            // Step the INPUT grid: stepping by the (smaller) display texel under upscale would
            // re-read the same input texel and collapse the variance box.
            float3 rgb = current.SampleLevel(uv + float2(x, y) * inputTexel, 0.0).rgb;
            float3 s = RGB_to_YCoCg(rgb);
            m1 += s;
            m2 += s * s;
        }
    }
    const float N = 9.0;
    float3 mu = m1 / N;
    float3 sigma = sqrt(max(m2 / N - mu * mu, float3(0.0, 0.0, 0.0)));
    float sigmaY = sigma.x;  // YCoCg luma std-dev — the thin-feature detector for lock creation

    // Dilated (closest-depth) reproject → the reprojected history UV. The dilation search walks
    // true INPUT texels (its motion / depth textures are input extent) and hands back the nearest
    // device depth for the disocclusion linearization.
    float closestDepth;
    float2 mv = DilatedMotion(uv, inputTexel, closestDepth);
    float2 histUv = uv + mv;
    float velPx = length(mv * push.screenSize);
    float lumaCur = Luma(cur);

    // Reactive mask (input extent, sampled by normalized uv across the ratio — never by tid): a
    // translucent / alpha-blended pixel whose reprojected history is unreliable.
    float react = saturate(reactive.SampleLevel(uv, 0.0).r * push.lockReactive.y);

    // Reconstruction state reprojected at histUv: .r lifetime, .g lock luma, .b prev linear depth.
    float4 lockPrev = lockHistory.SampleLevel(histUv, 0.0);
    float lockLife = max(lockPrev.r - 1.0, 0.0);   // age the lock one frame
    float lockLuma = lockPrev.g;

    // Parallax disocclusion: this frame's dilated closest depth vs the reprojected previous linear
    // depth (lock .b). A gross relative jump — a moving foreground revealing background — exceeds
    // the threshold; camera-only translation stays under it. Composes with (never replaces) the
    // variance clip; it catches the reveals the 3x3 box misses.
    float curLin = LinearizeDepth(closestDepth, push.depthParams);
    float prevLin = lockPrev.b;
    bool disoccluded = abs(curLin - prevLin) > push.disoccl.x * max(curLin, 1e-3);

    // Lock update: break on a shading change (current luma disagrees with the captured lock luma)
    // or a disocclusion; (re)create on a stable thin feature (strong neighbourhood luma contrast +
    // low velocity) that is not reactive/translucent (its motion is unreliable). The break happens
    // before the feedback floor below, so a lock never sticks history through a real change.
    float shadingChange = abs(lumaCur - lockLuma) / max(lumaCur, max(lockLuma, 0.2));
    if (disoccluded || shadingChange > push.disoccl.y)
    {
        lockLife = 0.0;
    }
    bool thinFeature = (sigmaY > 0.15) && (velPx < 2.0) && (react < 0.5);
    if (lockLife <= 0.0 && thinFeature)
    {
        lockLife = push.lockReactive.x;
        lockLuma = lumaCur;
    }
    bool locked = lockLife > 0.0;

    // Lock-scaled YCoCg variance clip: widen the half-extent while locked so a jittered thin
    // feature the 3x3 box would otherwise reject stays stable across frames.
    float gamma = push.gammaValid.x * (locked ? 1.75 : 1.0);
    float3 aabbMin = mu - gamma * sigma;
    float3 aabbMax = mu + gamma * sigma;
    float3 hist = SampleHistoryCatmullRom(history, histUv, float2(width, height));
    hist = YCoCg_to_RGB(clip_aabb(aabbMin, aabbMax, RGB_to_YCoCg(hist)));

    // Validity: an off-screen reprojection, an invalid (first) frame, or a disocclusion → take the
    // current frame whole.
    bool onScreen = histUv.x >= 0.0 && histUv.x <= 1.0 && histUv.y >= 0.0 && histUv.y <= 1.0;
    bool validHist = push.gammaValid.y >= 0.5 && onScreen && !disoccluded;

    // Accumulation confidence (upsampling): the reprojected sample count rides history alpha and
    // grows by one per valid frame toward sampleTarget. A freshly-covered — or disoccluded — display
    // pixel leans on the Lanczos current estimate and converges over the (ratio-scaled) jitter cycle.
    float sampleTarget = push.upscale.y;
    float histSamples = validHist ? history.SampleLevel(histUv, 0.0).a : 0.0;
    float samples = validHist ? min(histSamples + 1.0, sampleTarget) : 1.0;
    float confidence = saturate(samples / sampleTarget);  // 0 fresh … 1 converged

    // Velocity- and shading-adaptive feedback (history's share), then the Phase-3 robustness
    // modulations in order: the lock lifts a floor to protect the thin feature; confidence still
    // caps genuinely fresh pixels so a fresh lock can't smear un-accumulated history; the reactive
    // mask pulls translucent pixels to the current frame; the validity gate forces current whole on
    // disocclusion / first frame.
    float lumaHist = Luma(hist);
    float velFac = saturate(velPx * push.rejectSharp.x);  // 0 still … 1 fast
    float lumaDiff = abs(lumaCur - lumaHist) / max(lumaCur, max(lumaHist, 0.2));
    float stable = 1.0 - saturate(lumaDiff);
    float feedback = lerp(push.feedback.y, push.feedback.x, velFac);  // [min, max] by velocity
    feedback *= stable * stable;                                      // distrust shading changes
    if (locked)
    {
        feedback = max(feedback, max(push.feedback.x, 0.95));  // a lock keeps history
    }
    feedback *= confidence;         // fresh pixels still take current (the lock cannot smear)
    feedback *= (1.0 - react);      // translucent pixels take current (no ghosting)
    feedback = validHist ? feedback : 0.0;

    // Karis 1/(1+luma) tonemap weighting — suppresses fireflies in the pre-tonemap linear HDR.
    float wCur = (1.0 - feedback) * (1.0 / (1.0 + lumaCur));
    float wHist = feedback * (1.0 / (1.0 + lumaHist));
    float3 resolved = (cur * wCur + hist * wHist) / max(wCur + wHist, 1e-5);
    // Color history carries the UNSHARPENED resolve + the accumulated sample count; the lock rolls
    // the reconstruction state (lifetime, lock luma, this frame's linear depth for next-frame
    // disocclusion). The sharpen is a display-time filter and must not accumulate.
    outHistory[tid.xy] = float4(resolved, samples);
    outLock[tid.xy] = float4(lockLife, lockLuma, curLin, 0.0);

    // Optional RCAS-style contrast-adaptive sharpen at DISPLAY extent (sharpness == 0 fully bypasses
    // it). The '+' taps are the display-grid reconstructed-current neighbours (not input-extent
    // fetches — sharpening a sub-display neighbour would fight the reconstruction), so this recovers
    // exactly the softness the Lanczos resample introduced. The lobe firms up as the upscale ratio
    // rises; at ratio 1 it is bit-identical to the native sharpen. The lobe self-limits near the
    // neighbourhood min/max and the result clamps non-negative so the linear-HDR buffer never
    // carries negative radiance. Feeds `outColor` only — the temporal history stays unsharpened.
    float3 outColorRgb = resolved;
    float sharpness = push.rejectSharp.y;
    if (sharpness > 0.0)
    {
        float3 b = resolved;
        float3 tN = ReconstructCurrent(current, uv + float2(0.0, -1.0) * texel, push.screenSize);
        float3 tS = ReconstructCurrent(current, uv + float2(0.0, 1.0) * texel, push.screenSize);
        float3 tW = ReconstructCurrent(current, uv + float2(-1.0, 0.0) * texel, push.screenSize);
        float3 tE = ReconstructCurrent(current, uv + float2(1.0, 0.0) * texel, push.screenSize);
        float ratio = push.upscale.x;                             // display / input, >= 1
        float upscale = saturate(sharpness + 0.15 * (ratio - 1.0));  // more upscale → firmer lobe
        float3 mn = min(min(tN, tE), min(tS, tW));
        mn = min(mn, b);
        float3 mx = max(max(tN, tE), max(tS, tW));
        mx = max(mx, b);
        float3 amp = saturate(min(mn, max(float3(0.0, 0.0, 0.0), 2.0 - mx)) / max(mx, 1e-5));
        float3 lobe = amp * (-1.0 / lerp(8.0, 5.0, upscale));
        float3 sharpened = (b + lobe * (tN + tE + tS + tW)) / (1.0 + 4.0 * lobe);
        outColorRgb = max(sharpened, float3(0.0, 0.0, 0.0));
    }
    outColor[tid.xy] = float4(outColorRgb, 1.0);
}
