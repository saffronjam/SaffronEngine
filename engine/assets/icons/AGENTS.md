# Editor icons

SVGs vendored from [Lucide](https://lucide.dev/icons/). `cargo run -p xtask -- shaders` copies the
whole `icons/` directory next to the host binary on every build (alongside `models/` and `fonts/`,
via `copy_asset_tree` in `engine/xtask/src/shaders.rs`). Vegetation catalog thumbnails embed the
plant, biome, and map SVGs in `saffron-assets`; other host paths do not consume the copied files.
The CEF/React editor ships its own icons via the `lucide-react` package, and the host's
in-viewport billboards are native flat-colored glyphs built as overlay geometry
(`build_scene_edit_billboards` in `engine/crates/host/src/overlay.rs`) — no textures, and the
renderer has no SVG-icon upload path.

Keep icon additions aligned with an actual asset or host rendering path.

## Adding a new icon (if reviving)

```sh
# From the repo root:
curl -fsSL https://raw.githubusercontent.com/lucide-icons/lucide/main/icons/<name>.svg \
     -o engine/assets/icons/<name>.svg
```

Browse names at https://lucide.dev/icons/ — the URL slug is the filename without `.svg`.

## Current icons

| File | Lucide name | Originally used for |
|---|---|---|
| `box.svg` | box | Mesh asset fallback |
| `image.svg` | image | Texture asset fallback |
| `file.svg` | file | Unknown asset fallback |
| `lightbulb.svg` | lightbulb | PointLight billboard |
| `flashlight.svg` | flashlight | SpotLight billboard |
| `camera.svg` | camera | Camera billboard |
| `eye.svg` | eye | Entity visibility toggle |
| `sprout.svg` | sprout | Plant asset thumbnail |
| `tree-pine.svg` | tree-pine | Biome asset thumbnail |
| `map.svg` | map | Vegetation-map asset thumbnail |
