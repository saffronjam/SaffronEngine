# saffron-assets — the asset server, cooking, and the render mirror

The live catalog wrapped in uuid-keyed GPU caches, the `.smat` material system and its codegen, the
thumbnail worker, project I/O, the model import/bake pipeline, `render_scene`, the journal-driven
GPU scene mirror, and the whole vegetation cooking and artifact-store layer. It sits on top of
`saffron-geometry` (byte codecs), `saffron-rendering` (`GpuMesh`/`GpuTexture` + the bindless table),
and `saffron-scene` (the catalog types and the ECS world).

Vegetation is the largest single area here, plus heavy coupling in `gpu_scene_mirror/`. The value
contracts it cooks belong to `saffron-vegetation` (`engine/crates/vegetation/AGENTS.md`); this crate
is where they meet the filesystem and the GPU.

## Layout

Every area that outgrew one file is a directory module (`mod.rs` plus cohesive siblings), so the
crate's public surface is unchanged by where an item lives.

| Area | Modules |
|---|---|
| Server core | `lib.rs` (the `AssetServer`, caches, reserved ids), `cache.rs`, `catalog.rs`, `load/` (`mesh`, `texture`, `builtin`), `scan/` (`reconcile`, `sidecar`, `texture`, `roles`) |
| Project + import | `project.rs`, `import/` (`bake`, `meta`), `model.rs`, `manage/` (`extract`, `reimport`, `references`, `clean`, `material_import`, `container`), `spawn.rs` |
| Materials | `material/` (`codec`, `overrides`, `io`), `render_material.rs`, `graph.rs`, `codegen.rs`, `thumbnail/` (`job`, `hash`, `cache`) |
| Render drive | `render_scene/` (`frame`, `gather`, `pick`, `celestial`), `gpu_scene_mirror/`, `mesh_surface.rs`, `journal.rs`, `page_stream.rs` |
| Vegetation cooking | `vegetation/`, `vegetation_cooker/`, `plant_cook/`, `cook_reader.rs`, `plant_render.rs` |
| Vegetation artifacts | `vegetation_store/`, `vegetation_state.rs`, `vegetation_export.rs` |

## Rules that are easy to break

- **The negative cache is a present key holding `None`.** The GPU caches are
  `HashMap<u64, Option<Arc<T>>>`, where a present `None` means "this load failed, do not retry" and
  an absent key means "never attempted". `resolve_cached` is the single path that honours the
  distinction. A new load path that treats `None` as absent retries a broken asset every frame.
- **Idle the GPU before clearing caches.** `clear_asset_caches` drops the maps; the last `Arc` drop
  runs the resource's `Drop`, frees the VMA allocation, and returns the bindless slot. An in-flight
  frame may still reference an `Arc<GpuTexture>`, so the *caller* must `wait_gpu_idle` first. This
  is call-site discipline that `Drop` ordering cannot enforce.
- **`gpu_scene_mirror/` also retains device `Arc`s.** It caches `Arc<GpuMesh>` and
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
- **CPU coverage has exactly one phase.** `CanonicalCpuCoverage` classifies at
  `CANONICAL_COVERAGE_PHASE`, never at the renderer's per-frame TAA phase. A surface-field answer
  reaches cooked vegetation bytes, and `SurfaceProviderDescriptor::revision` does not cover the
  coverage record — so a phase that moved per frame would change published bytes while every cook
  key stayed identical, which no cache check can catch. The raster path advances its own phase
  (`gpu_scene_upload`, `instancing.rs`); that one is unrelated to this one.
- **`scan_assets` skips the `.cache/` subtree.** The filesystem is the source of truth for the
  catalog; `assets/.cache/catalog.json` is a fast path, not a second truth. Do not add a scan path
  that walks into it.

### Vegetation artifacts

- **The store is content-addressed and lives beside `assets/`, not inside it.**
  `project_vegetation_cache_root` resolves to `<project>/cache/vegetation/`, and
  `VegetationArtifactKind` maps the five namespaces to `plants/`, `cells/`, `manifests/`,
  `cook-graphs/`, and `work-payloads/`. Publication is atomic (`AtomicWriteFile`).
- **Persistent state has its own root and never enters the store.** `VegetationStateStore`
  (`project_vegetation_state_root` → `<project>/state/vegetation/baselines/`) owns the one thing here
  that no authored source reproduces. Everything in the artifact cache is rebuildable byte for byte,
  so it may be deleted at any time; a baseline is a snapshot of runtime mutations, so putting it under
  the cache root would make clearing a cache destroy authored work. Nothing that publishes or reads a
  baseline may resolve it through `VegetationArtifactStore` —
  `deleting_the_artifact_cache_loses_no_authored_or_persistent_bytes` in `vegetation_state.rs` is the
  tripwire, and it removes the cache root on disk.
- **Any packaging path must copy both roots.** They are outside `assets/`, so a packager that copies
  only `assets/` produces a player that binds no manifest and comes up bare, and one that copies only
  the cache produces a world that boots untouched — with nothing in the tree catching either.
- **The export closure comes from the manifest, never from a directory scan.**
  `vegetation_export_closure` walks generation root → manifest → every compiled family and cell the
  manifest names, and reports the durable state separately in `state_files` because it lands under a
  different root. A scan copies every generation the project ever cooked, including superseded ones,
  and quietly multiplies package size.
- **An artifact's filename is the hash of its bytes**, so `verify_vegetation_artifacts` is a rehash
  and needs no side table that could itself rot. Only content-addressed kinds are rehashed: a
  generation root is *keyed* by the map it belongs to, and rehashing it reports every root as corrupt.
  **Repair deletes rather than rewrites** — the bytes are the only copy, and the cooker's cache-miss
  path is what reproduces them. Verification never touches `state_files`: a baseline is the only copy
  of something no cook reproduces, so "delete the corrupt one" is the wrong repair for it.
- **A baseline is keyed by its authored map, one per map**, and names inside itself the generation it
  was reduced against. Keying the file by the generation instead orphans it on the next recook — a new
  identity finds no baseline, and the world comes up bare with every delta still on disk. Publishing
  validates that the snapshot decodes against the generation it claims; binding compares that claim to
  the bound generation and rebases when they differ (`VegetationState::rebase`), never silently
  starting empty.
- **Authored vegetation is excluded from a packaged `assets/`.** A player never reads a `.splant`,
  `.sbiome`, or `.svegmap`; it binds by identity through the artifact store. `.svegcell` and
  `.splantc` are never catalog assets and cannot be imported or edited.
- **A warning must describe the case worth warning about.** The export path warns when a project
  declares vegetation maps but has no cooked generation — not when it simply has no vegetation.

## Tests

**One convention: a module's tests are an inline `#[cfg(test)] mod tests { … }` in the file that
owns the items, and fixtures shared across a directory module's siblings live in its
`test_support.rs`.** No `#[path]` redirection, no `*_tests.rs` sidecars, no second name for the
fixture module. One integration test, `tests/smat_golden_snapshot.rs`, uses the repo's on-disk
golden mechanism (`saffron-test-support::assert_bytes_match_golden`) — unlike `saffron-vegetation`,
which keeps its goldens as inline literals. Device-backed tests need the Vulkan environment;
`cargo test` without it silently skips them, so a green run proves nothing about the GPU paths.
