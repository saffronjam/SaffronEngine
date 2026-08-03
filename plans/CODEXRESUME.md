# Resume: meshes render as fragments

**Status:** IN PROGRESS — root cause not isolated. Investigation paused mid-flight.

Every mesh loses most of its triangles in the camera view. Reproduced headless, deterministically.

## The finding that matters

A **built-in primitive sphere** renders as a torn crescent sliver, in the same frame where an
imported chair renders as scattered fragments. A primitive never touches the importer, the cooker,
or the asset catalog — so this is **not the asset pipeline**. It is the raster path every mesh
shares.

That crescent is the same shape as the torn material-preview sphere. The chair fragments, the torn
preview, the blank model thumbnails and the viewport flicker are very likely **one bug**, not four.

## Proven healthy (do not re-investigate)

- **The cooked asset.** Decoding the real `BarberShopChair_01_2k` `.smesh`: root bounds ==
  prototype bounds == decoded triangle extent, exactly (`x -0.394..0.363`, `y 0..1.489`,
  `z -0.442..0.886`). 165 nodes, 6 levels (`{0:1, 1:1, 2:2, 3:8, 4:31, 5:122}`), 581 clusters,
  1 voxel node. Bounded branching working.
- **The cooker.** A radius-1 sphere fixture cooks with root bounds, leaf union and unorm-decoded
  cluster vertices all exactly `[-1, 1]`.
- **The LOD cut and page streaming.** `coarse` → 1 voxel record; `fine` → 122 leaf records;
  `auto` → 125 records at depth 5. Streaming drains to 184/220 resident, `requested`/`loading` 0.
  Zero occlusion culls, zero validation errors. This part of the earlier fix held.
- **The instance transform.** Identity, scale 1, chair at origin.

## The symptom, quantified

Camera 3 m from a 1.5 m chair, 45° FOV, 1600x900 → it should cover **~540 px**. It renders as a
**~40 px** mangled blob. So it is a size/placement error as well as missing triangles — not merely
"some clusters dropped".

Draw records *are* emitted (125 of them). The draws happen; the triangles do not land.

## Remaining suspects (all in the shared raster path)

1. The executor's indexed-MDI vertex pull — `geometry.vertexStride`, `geometry.vertices.first`
   (`motion.slang` / `mesh.slang` / `gbuffer.slang`, `vertexMainExecutor`).
2. Per-cluster index-blob offsets — `GpuPageClusterRecord.firstIndex/indexCount` vs the blob
   written in `engine/crates/rendering/src/page_payload.rs`.
3. Cluster cone / backface culling — cone data comes from `compute_meshlet_bounds` in
   `cook.rs::build_clusters`. A primitive sphere reduced to a crescent looks a lot like most
   clusters being rejected, or fetched from the wrong offset.

Related and probably the same family: the `ERROR_DEVICE_LOST` after flying around reports
`device fault address: READ_INVALID at 0x0` with the `motion` pass wedged — a **null
buffer-device-address dereference on the GPU**, i.e. the vertex path being handed a zero pointer.

## Reproduce

Copy `appdata/userdata/test` to a scratch dir (do not work against the real project), then boot
headless and drive it over the control plane:

```sh
export VK_ADD_DRIVER_FILES=/run/host/usr/share/vulkan/icd.d/nvidia_icd.x86_64.json
export SAFFRON_EDITOR_NATIVE_VIEWPORT=1
export SAFFRON_CONTROL_SOCK=/run/user/1000/sa-repro.sock
export SAFFRON_PROJECT=<scratch copy of appdata/userdata/test>
./engine/target/debug/saffron-host &

sa scan-assets
sa list-assets                                    # take the row with type == "model"
sa add-entity --preset sphere                     # the control: a built-in primitive
sa set-transform --entity <sphere> --translation "[1.2,0.75,0]"
sa instantiate-model --asset <model-id>           # NOTE: --asset, not --asset-id
sa deselect
sa set-camera --pivot "[0.6,0.75,0.22]" --distance 3.5 --near 0.01
sa gpu-scene-stats
sa screenshot --path both.png
```

Both the primitive and the model come out fragmentary. `sa set-hierarchy-cut --cut fine --view
camera` (the `--cut/--view` form; the positional form its help documents is broken) forces the
leaf-only cut and does **not** fix it, which is what rules the LOD cut out.

## Dead ends and corrections

- `instantiate-model` takes `--asset`, **not** `--asset-id`. Two of my earlier runs used the wrong
  flag and therefore rendered a scene with **no model in it at all**; the "giant bounds" I inferred
  from those images is not real. Discard any conclusion drawn from an empty scene.
- Not displacement: rendering with `set-displacement --enabled false` is pixel-identical.
  `displacedRecords` does not exist on the control plane, so that toggle was never observable
  through `render-stats` anyway.
- Not streaming latency, not the traversal stack depth, not `appearanceTotal` — all measured clean
  above.

## Tree state

All work is **unstaged**; nothing committed, staged, or pushed. Two temporary probe tests
(`zz_probe_bounds` in `cook.rs`, `zz_probe_real_asset` in `smodel.rs`) were added to take the
measurements above and have been **removed** — both files are back to their intended contents.

Still-open smaller items inherited from the earlier fix pass: dead `Error::NotConverged` variant,
orphaned doc comment at `renderer/gpu_scene.rs:70`, `calibrate_voxel_appearance_error` running
after `assign_pages` (widening a node now stales its children's `transition_error`), and no e2e
coverage for mesh/model thumbnails.
