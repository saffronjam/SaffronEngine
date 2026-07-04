# Texture role data model — persist it, plumb it, surface it

**Status:** IMPLEMENTED. `saffron-scene` gained a `TextureRole` enum + `AssetEntry.role`; the role is
inferred from the filename token (`infer_texture_role`) and persisted through `.smeta`
(`SmetaData.role`) and the catalog cache JSON, surviving cold scans. `import-texture` gained a `role`
hint (`texture_role_from_hint`, canonical-first so a connector `"normal"` dodges the `detect`
`"normal"⊃"orm"` quirk); the **engine now owns the role→colorspace policy** (`colorspace_for_role_explicit`)
and the editor's duplicated `colorspace_for_role` is deleted — connectors send the role, the engine
derives the upload space (color→sRGB, HDRI→float, every other explicit role→linear) *and* stores the
role. `AssetEntryDto` exposes `colorspace` + `role`. Verified: build + `clippy -D warnings` + `fmt`
clean, `texture_role_names_round_trip` + role-inference/round-trip scan tests pass in isolation,
frontend `tsc` + Tauri-bridge `cargo check` clean. (The shared GPU test fixture SIGSEGVs at Vulkan
device teardown when several GPU tests share a process — a pre-existing environmental fixture leak, not
this change; each test's assertions pass alone.)
**Scope:** `saffron-scene`, `saffron-assets`, `saffron-protocol`, `saffron-control`, editor connectors
**Depends on:** — (independent; start early, in parallel with `primitive-meshes/`)

## Goal

The editor can reliably tell a texture's role apart (albedo / normal / roughness / metallic / ao /
height / emissive / opacity / hdr) so it can route the right preview. The engine already knows
colorspace end-to-end and *computes* role at import; it just discards role and never surfaces either.

## Touch points

- **`scene/src/environment.rs`** — add `role: TextureRole` (a new enum) to `AssetEntry` alongside
  `hdr`/`linear`/`colorspace`. `TextureRole { Albedo, Normal, Roughness, Metallic, Ao, Height,
  Emissive, Opacity, Orm, Hdri, Unknown }`.
- **`assets/src/scan.rs`** — `detect_material_role` already produces the value (its only caller is
  `manage.rs` `import_material_folder`); **stop discarding it** — write it onto the `AssetEntry` and
  into the `.smeta` sidecar (`SmetaData`, beside `type`/`colorspace`). A stored role can also fix the
  foreign-drop colorspace guess (data maps currently default to `Srgb`, which is wrong for
  normal/rough/etc.).
- **`protocol/src/dto.rs`** — extend `ImportTextureParams` with an optional `role`. Add optional
  `colorspace?: "srgb"|"linear"|"hdr"` and `role?: string` to `AssetEntryDto`; populate in `asset_dto`
  (`control/src/commands_asset.rs`). Run `xtask gen-protocol` / `bun run check`.
- **editor connectors** (`editor/src-tauri/src/connectors/`, `lib.rs`) — the connector *knows*
  "normal" vs "roughness" (`polyhaven.rs`, `ambientcg.rs`) but `store_import_part` collapses it via
  `colorspace_for_role` and sends only `{ path, colorspace }`. Pass `part.role` through the extended
  `ImportTextureParams` — the most authoritative signal available.

## Verification

- A connector-imported normal-only map and roughness-only map land with distinct `role`s on the row and
  in `.smeta`; a reload preserves them.
- `AssetEntryDto` carries `role`/`colorspace`; `@saffron/protocol` regenerated; editor sees them.
- Foreign drop of `wood_nrm.png` is tagged `Normal` + `Linear`, not `Srgb`.

## Risks

- **Heuristic reliability.** `detect_material_role` is a filename-substring guess; oddly-named
  hand-drops get `Unknown` and fall to the flat-image fallback. Connector imports are authoritative.
  A per-texture role **override** in the Inspector (mirroring the existing colorspace override) is the
  escape hatch — decide v1 vs deferred (see README open decisions).
