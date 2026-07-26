# saffron-assets — the asset server, cooking, and the render mirror

The live catalog wrapped in uuid-keyed GPU caches, the `.smat` material system and its codegen, the
thumbnail worker, project I/O, the model import/bake pipeline, `render_scene`, the journal-driven
GPU scene mirror, and the whole vegetation cooking and artifact-store layer. It sits on top of
`saffron-geometry` (byte codecs), `saffron-rendering` (`GpuMesh`/`GpuTexture` + the bindless table),
and `saffron-scene` (the catalog types and the ECS world).

Vegetation is the largest single area here — roughly 11,700 lines across seven files, plus heavy
coupling in `gpu_scene_mirror.rs`. The value contracts it cooks belong to `saffron-vegetation`
(`engine/crates/vegetation/AGENTS.md`); this crate is where they meet the filesystem and the GPU.

## Layout

| Area | Files |
|---|---|
| Server core | `lib.rs` (the `AssetServer`, caches, reserved ids), `cache.rs`, `load.rs`, `catalog.rs`, `scan.rs` |
| Project + import | `project.rs`, `import.rs`, `model.rs`, `manage.rs`, `spawn.rs` |
| Materials | `material.rs`, `render_material.rs`, `graph.rs`, `codegen.rs`, `thumbnail.rs` |
| Render drive | `render_scene.rs` (the highest-coupling driver, plus `pick_entity`), `gpu_scene_mirror.rs`, `mesh_surface.rs`, `journal.rs`, `page_stream.rs` |
| Vegetation cooking | `vegetation.rs`, `vegetation_cooker.rs`, `plant_cook.rs`, `cook_reader.rs`, `plant_render.rs` |
| Vegetation artifacts | `vegetation_store.rs`, `vegetation_export.rs` |

## Rules that are easy to break

- **The negative cache is a present key holding `None`.** The GPU caches are
  `HashMap<u64, Option<Arc<T>>>`, where a present `None` means "this load failed, do not retry" and
  an absent key means "never attempted". `resolve_cached` is the single path that honours the
  distinction. A new load path that treats `None` as absent retries a broken asset every frame.
- **Idle the GPU before clearing caches.** `clear_asset_caches` drops the maps; the last `Arc` drop
  runs the resource's `Drop`, frees the VMA allocation, and returns the bindless slot. An in-flight
  frame may still reference an `Arc<GpuTexture>`, so the *caller* must `wait_gpu_idle` first. This
  is call-site discipline that `Drop` ordering cannot enforce.
- **`gpu_scene_mirror.rs` also retains device `Arc`s.** It caches `Arc<GpuMesh>` and
  `Arc<GpuTexture>` per entry, so a teardown that clears the uploader and the asset caches but
  leaves the mirror populated keeps `DeviceResources` alive past instance destroy — which surfaces
  as a MoltenVK abort at `vkDestroyInstance`, far from the cause. Clear every cache that holds
  device handles.
- **`GpuSceneMirror` is a derived view, never authority.** It is the one bridge from canonical
  engine state to the persistent GPU scene, driven by the mutation journal. It must never become a
  place where scene or vegetation truth is stored or repaired; a cache rebuild reconstructs it from
  the journal and the catalog.
- **A reserved-id asset is answered analytically, not seeded into a cache.**
  `EDITOR_CAMERA_MATERIAL_ID` is handled by a branch in `render_material.rs`. Seeding a reserved id
  into `material_by_uuid` as a side effect of some other work means any wholesale cache clear
  permanently loses it, and the sibling mesh cache still hitting hides the failure.
- **`scan_assets` skips the `.cache/` subtree.** The filesystem is the source of truth for the
  catalog; `assets/.cache/catalog.json` is a fast path, not a second truth. Do not add a scan path
  that walks into it.

### Vegetation artifacts

- **The store is content-addressed and lives beside `assets/`, not inside it.**
  `project_vegetation_cache_root` resolves to `<project>/cache/vegetation/`, and
  `VegetationArtifactKind` maps the five namespaces to `plants/`, `cells/`, `manifests/`,
  `cook-graphs/`, and `baselines/`. Publication is atomic (`AtomicWriteFile`).
- **Any packaging path must copy that directory.** It is outside `assets/`, so a packager that
  copies only `assets/` produces a player that binds no manifest and comes up bare — with nothing in
  the tree catching it.
- **The export closure comes from the manifest, never from a directory scan.**
  `vegetation_export_closure` walks generation root → manifest → every compiled family and cell the
  manifest names. A scan copies every generation the project ever cooked, including superseded ones,
  and quietly multiplies package size.
- **An artifact's filename is the hash of its bytes**, so `verify_vegetation_artifacts` is a rehash
  and needs no side table that could itself rot. Only content-addressed kinds are rehashed: a
  generation root and a baseline are *keyed* by what they belong to, and rehashing them reports
  every baseline as corrupt. **Repair deletes rather than rewrites** — the bytes are the only copy,
  and the cooker's cache-miss path is what reproduces them.
- **A baseline is keyed by its manifest, one per generation.** A second baseline for the same
  generation is an ambiguity nothing resolves. A baseline that does not decode against its
  generation is a hard error at publish and at bind, not a warning.
- **Authored vegetation is excluded from a packaged `assets/`.** A player never reads a `.splant`,
  `.sbiome`, or `.svegmap`; it binds by identity through the artifact store. `.svegcell` and
  `.splantc` are never catalog assets and cannot be imported or edited.
- **A warning must describe the case worth warning about.** The export path warns when a project
  declares vegetation maps but has no cooked generation — not when it simply has no vegetation.

## Tests

Inline `#[cfg(test)] mod tests` plus dedicated `*_tests.rs` modules (`scan_tests`, `project_tests`,
`spawn_tests`, `import_tests`), and one integration test, `tests/smat_golden_snapshot.rs`, which
uses the repo's on-disk golden mechanism (`saffron-test-support::assert_bytes_match_golden`) —
unlike `saffron-vegetation`, which keeps its goldens as inline literals. Device-backed tests need
the Vulkan environment; `cargo test` without it silently skips them, so a green run proves nothing
about the GPU paths.
