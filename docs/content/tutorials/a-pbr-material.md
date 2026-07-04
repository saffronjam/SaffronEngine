+++
title = 'PBR material'
weight = 3
math = true
+++

# PBR material

Turn a plain mesh into a believable metal. You set its base color, push metallic to 1 and
roughness down, add a point light for the specular highlight to reflect, and tune the
tonemap exposure so the bright result does not clip. Every value feeds the Cook-Torrance
BRDF the engine shades with. Start from [your first scene](../your-first-scene/) or any
scene with one mesh entity.

## Set base color and metallic-roughness

A mesh's look comes from its `MaterialSet`: each slot references a `.smat`
[material](../../explanations/materials-and-pipelines/native-materials/) asset and layers
per-object overrides on top. Give the cube a dedicated material so the PBR knobs — a base color,
`metallic`, and `roughness` — live in one editable asset, then assign it to the mesh:

```sh
just sa material-create --name Gold
just sa material-assign --entity Mesh --material Gold
```

`material-update` merges the factors you name onto the material and leaves the rest untouched, so
you can set each knob without resetting the others:

```sh
just sa material-update --material Gold \
  --baseColor '{"x":0.95,"y":0.64,"z":0.22,"w":1}' --metallic 1 --roughness 0.25
```

That makes the cube gold: warm base color, fully metallic, fairly smooth. The metallic flag
routes the base color into the Fresnel reflectance $F_0$ instead of the diffuse albedo, so a
metal reflects its own color and carries no Lambertian term:

$$
F_0 = \operatorname{lerp}(0.04,\ \text{baseColor},\ \text{metallic})
$$

Roughness controls how tight the highlight is; lower is sharper. In the editor, open **Gold** in the
Material panel and edit **Base Color**, **Metallic**, and **Roughness** against the live preview
sphere (Metallic and Roughness are `0..1` sliders); to change only this cube instead, override the
same knobs in the Inspector's **MaterialSet** slot. Read back what you set:

```sh
just sa material-get --material Gold        # shows baseColor, metallic, roughness
```

> [!NOTE]
> Roughness is clamped to `[0.045, 1.0]` in the shader: a true zero collapses the GGX
> denominator into an infinitely sharp highlight. See
> [the Cook-Torrance BRDF](../../explanations/lighting-and-brdf/cook-torrance-brdf/).

## Add a point light to reflect

A metal shows only what it reflects. The directional sun gives one broad highlight; a
nearby point light gives a tight one that moves as you orbit. Place one above and to the
side:

```sh
just sa create-entity Key Light
just sa add-component "Key Light" PointLight
just sa set-transform "Key Light" --translation '{"x":2,"y":3,"z":2}'
just sa set-component "Key Light" PointLight --json '{"intensity":20,"range":15,"color":{"x":1,"y":1,"z":1}}'
```

`set-component` writes the exact JSON shape used in scene files, setting the whole
`PointLight` at once. In the editor: **Create ▸ Point Light**, then edit
**Intensity**, **Range**, and **Color** in the Inspector. Point lights are culled per froxel
by the clustered-forward path, so adding more costs almost nothing per pixel.

Confirm the renderer is feeding lights:

```sh
just sa render-stats        # clustered=on; the fragment loops only its cluster's lights
```

## Tune exposure so the highlight doesn't clip

Lighting is computed in linear HDR, then tonemapped to the display. A bright metal under an
intensity-20 light can blow out to white. The tonemap takes an exposure in stops (EV),
applied as $2^{EV}$ before the curve, so negative EV darkens:

```sh
just sa set-exposure -1.5      # pull the image down 1.5 stops
just sa screenshot viewport /tmp/gold.png
```

Sweep it and compare PNGs: `set-exposure 0` is neutral, `-2` is much darker, `+1` brightens.
Pick the value where the highlight reads as a bright spot rather than a flat white patch.
`render-stats` reports the current `exposureEv`.

> [!NOTE]
> Exposure is a global render setting, not per-material, so there is no Inspector field for
> it — drive it from the CLI. See
> [HDR and exposure](../../explanations/lighting-and-brdf/hdr-and-exposure/).

## Make it glow (optional)

`emissive` adds light the surface emits on its own, independent of any scene light. Give
the Gold material a faint emissive tint:

```sh
just sa material-update --material Gold --emissive '{"x":0.2,"y":0.1,"z":0}' --emissiveStrength 2
```

Emissive radiance is added after lighting, so it shows even in shadow. In the **Material** panel it's
**Emissive** (a color) and **Emissive Strength** (a multiplier). Set the strength to 0 to
turn it off.

## Save and next steps

Save the look into your project so it persists:

```sh
just sa save-project /tmp/first-scene/project.json
```

- [Cook-Torrance BRDF](../../explanations/lighting-and-brdf/cook-torrance-brdf/) — the math your metallic/roughness drive.
- [Punctual lights and attenuation](../../explanations/lighting-and-brdf/punctual-lights-and-attenuation/) — how a point light's intensity and range fall off.
- [HDR and exposure](../../explanations/lighting-and-brdf/hdr-and-exposure/) — what `set-exposure` feeds.
- [Image-based lighting](../../explanations/image-based-lighting/ibl-overview/) — turn on `set-ibl 1` for an environment ambient that metals reflect.
- [Übershader](../../explanations/materials-and-pipelines/ubershader-and-specialization/) — how `unlit` selects a different pipeline.
