+++
title = 'Night sky'
weight = 6
math = true
+++

# Night sky

The night sky combines catalog stars, an equatorial Milky Way, a geometrically phased Moon, and low-light vision in the HDR rendering path. Its radiance passes through the same atmosphere used by the daytime sky.

## Catalog stars

Anima bakes 9,096 records from the [Yale Bright Star Catalog](http://tdc-www.harvard.edu/catalogs/bsc5.html) into `bsc5.bin`. Each record carries a J2000 direction, HDR luminance, and linear RGB color. Apparent magnitude $m$ becomes luminance through the Pogson ratio:

$$
L(m)=2.512^{7-m}.
$$

A five-magnitude difference is about a factor of 100 in radiance. Spectral class supplies an approximate blackbody temperature, which the baker converts through CIE chromaticity to linear sRGB. Magnitude controls radiance rather than point size.

`stars.slang` expands every catalog row into a fixed screen-space quad with a Gaussian point-spread function. The fixed footprint keeps dim stars from becoming large sprites and gives bloom HDR radiance to work with.

## Sidereal rotation and the Milky Way

Catalog directions and the Milky Way cube share the J2000 equatorial frame. `world_from_equatorial` rotates that frame into local east-up-north coordinates from Julian date, observer latitude, and local mean sidereal time. The sky therefore turns with the calendar even when manual celestial control is active.

The Milky Way cube models a narrow galactic plane, a broad stellar band, a central bulge, and dust attenuation. `fragmentMain` samples it in equatorial coordinates behind the procedural environment.

## Atmosphere composition

A star contributes its catalog radiance multiplied by atmosphere transmittance in its viewing direction:

$$
L_{star}=L_{catalog}\,C_{spectral}\,P_{PSF}\,T(\omega).
$$

The star pass adds this background radiance to the HDR target that already contains sky in-scatter. The Milky Way follows the same transmittance path. Daylight and twilight wash out both signals through physical sky radiance; there is no Sun-elevation alpha fade. Without an active atmosphere, transmittance becomes the neutral value one.

## Moon and low-light vision

The lunar ephemeris supplies the Moon direction, and its separation from the Sun gives the illuminated fraction. `atmos_skygen.slang` draws the disc with a Lommel-Seeliger response, an opposition term, blue earthshine on the dark side, and atmosphere transmittance. The phase changes as the scene date advances.

Night adaptation is part of the mandatory tonemap pass. The [low-light tone-mapping model of Kirk and O'Brien](https://dl.acm.org/doi/10.1145/2010324.1964937) motivates the rod/cone blend and Purkinje blue shift. `scotopicAdapt` applies more rod response to dim pixels while bright emissive detail keeps its cone color; the operation is an identity during daylight.

## Example

These two settings keep the same date and location while moving from pre-dawn to noon:

```sh
sa set-time-of-day --enabled true --dayLengthSeconds 0 --timeOfDay 0.05
sa set-time-of-day --timeOfDay 0.5
```

The first frame contains atmosphere-extincted stars and a phased Moon. The second keeps the same background radiance inputs, but daylight sky radiance dominates them.

## In the code

| What | File | Symbols |
|---|---|---|
| Catalog baker | `engine/xtask/src/stars.rs` | `bake`, `parse_record`, `blackbody_linear_srgb` |
| Runtime catalog and Milky Way | `engine/crates/rendering/src/stars.rs` | `StarCatalog`, `record_stars`, `build_milky_way_cube` |
| Star point-spread draw | `engine/assets/shaders/stars.slang` | `vertexMain`, `fragmentMain` |
| Milky Way composition | `engine/assets/shaders/sky.slang` | `fragmentMain`, `worldFromEquatorial` |
| Moon phase and earthshine | `engine/assets/shaders/atmos_skygen.slang` | `computeMain`, `lommelSeeliger`, `earthshine` |
| Low-light response | `engine/assets/shaders/tonemap.slang` · `engine/assets/shaders/tonemap_ops.slang` | `computeMain`, `scotopicAdapt` |

## Related

- [Time of day](../time-of-day/) — calendar playback, ephemerides, and appearance curves
- [Procedural atmosphere](../procedural-atmosphere/) — the transmittance and sky in-scatter ledgers
- [Real-time sky-light capture](../realtime-skylight-capture/) — SH ambient and specular capture from the moving sky
