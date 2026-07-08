# Phase 1 — Graph vocabulary + indirect plumbing + capability probes + keyed transient acquire

**Status:** COMPLETED

## Goal

Land every render-graph primitive, capability probe, and transient-pool upgrade the later phases need,
with **zero behaviour change**, so the milestone gate (`just engine` + `just prepare-for-commit`) stays
green and every addition ships already unit-tested on CPU. Nothing here is load-bearing yet: the two new
`RgUsage` rows have no consumer, the `add_indirect_compute_pass` helper is never called, the three
capability probes are stored-but-unread, and the keyed `TransientResources` acquire has zero consumers
(it is dormant today, `transient.rs` has no live caller). This is the foundation the amplifying
tessellator (Phase 4), the indirect raster consumption (Phase 6), and the portable RT floor (Phase 7)
build on — laid down first so those phases add *only* behaviour, never plumbing.

The tree today has **no** indirect dispatch or draw, **no** counter/args buffer, **no** `INDIRECT_BUFFER`
buffer usage, and **no** `DRAW_INDIRECT`/`INDIRECT_COMMAND_READ` entry in the graph's golden usage table
(confirmed: the only indirect commands anywhere are the ones this phase makes *callable*, not called).

## Build plan

### 1. Two new `RgUsage` rows (`crates/rendering/src/render_graph.rs`)

The graph's single source of truth for barriers is the `RgUsage` enum (lines 36–57) → the `usage_info`
golden table (lines 265–329) → `apply_access` (line 365). A pass declares usage; the graph derives every
`vkCmdPipelineBarrier2`. Add two read-only rows, mirroring the `VertexInputRead` / `AccelStructBuildRead`
rows already present (enum at lines 54–56, table at lines 316–327):

- **`RgUsage::IndexInputRead`** — a buffer read as the index stream by a draw:

  ```
  stage:    vk::PipelineStageFlags2::INDEX_INPUT
  access:   vk::AccessFlags2::INDEX_READ
  layout:   vk::ImageLayout::UNDEFINED   // buffer usage, no layout
  is_write: false
  ```

- **`RgUsage::IndirectCommandRead`** — a buffer read as indirect dispatch/draw args (or draw-count):

  ```
  stage:    vk::PipelineStageFlags2::DRAW_INDIRECT
  access:   vk::AccessFlags2::INDIRECT_COMMAND_READ
  layout:   vk::ImageLayout::UNDEFINED
  is_write: false
  ```

  (`DRAW_INDIRECT` is the correct stage for **both** `vkCmdDispatchIndirect` and the indirect draws — the
  spec's `VK_PIPELINE_STAGE_2_DRAW_INDIRECT_BIT` covers "where indirect draw/dispatch data is consumed".)

The barrier core needs **no structural change**: `apply_access` (line 365) is generic over `RgUsageInfo`
and already handles arbitrary buffer read-after-write / write-after-read hazards; `derive_pass_barriers`
(line 531) and `execute_profiled` (line 585) fold whatever rows the table returns. With these two rows,
the Phase-4 tessellator declaring `StorageWriteCompute` on the generated index / indirect-args buffers and
the Phase-6 consumers declaring `IndexInputRead` / `IndirectCommandRead` on the same resources produce the
correct `COMPUTE_SHADER,SHADER_STORAGE_WRITE → INDEX_INPUT,INDEX_READ` and
`… → DRAW_INDIRECT,INDIRECT_COMMAND_READ` barriers automatically — the same derivation that already orders
the `displace` compute write ahead of every `VertexInputRead` consumer.

Extend the two existing tests in the `tests` module (line 746):

- **`usage_info_matches_the_golden_table`** (line 769) — append the two new `(usage, stage, access, layout,
  is_write)` cases to the `cases` array (mirror the `VertexInputRead` case at lines 828–834), so the table
  is asserted exhaustively.
- A **hazard test** mirroring `buffer_memory_barrier_on_hazard_only` (line 950): a `StorageWriteCompute`
  write followed by an `IndexInputRead` read yields exactly one memory barrier with
  `src = COMPUTE_SHADER,SHADER_STORAGE_WRITE` / `dst = INDEX_INPUT,INDEX_READ`, and no image barrier
  (buffers never emit image barriers). A second case does the same for `IndirectCommandRead`
  (`dst = DRAW_INDIRECT,INDIRECT_COMMAND_READ`) — this **is** the "no-op indirect pass round-trip": a
  producer write ordered ahead of an indirect-args read, proven at the derivation layer with no device.

### 2. `INDIRECT_BUFFER` usage flows through `Buffer::new` (`crates/rendering/src/resources.rs`)

`Buffer::new` (line 127) already takes an arbitrary `usage: vk::BufferUsageFlags`, so
`vk::BufferUsageFlags::INDIRECT_BUFFER` passes through unchanged — no signature change. The concrete
requirement is that the **usage union** the Phase-3 args/count buffer will be acquired under includes
`INDIRECT_BUFFER` (the flag no buffer in the tree carries today). That union is
`STORAGE_BUFFER | INDIRECT_BUFFER | SHADER_DEVICE_ADDRESS` for the dispatch/draw args + count buffer, and
`STORAGE_BUFFER | VERTEX_BUFFER | INDEX_BUFFER | INDIRECT_BUFFER | SHADER_DEVICE_ADDRESS |
ACCELERATION_STRUCTURE_BUILD_INPUT_READ_ONLY_KHR` for the shared tessellated VB/IB (matching
`skinning.rs:make_deformed_buffer`'s RT flags). Phase 1 only proves the flag is acquirable: the keyed
transient test (§5) acquires a buffer with `INDIRECT_BUFFER` in its usage and asserts reuse hits, so the
allocator path is exercised. No live pass acquires an indirect buffer yet.

### 3. `add_indirect_compute_pass` helper (`crates/rendering/src/renderer.rs`)

Add a sibling to `add_compute_pass` (line 6711). `add_compute_pass` records
`raw_body.cmd_dispatch(cmd, groups_x, groups_y, 1)` inside a `RgPass::compute(name).body(…)` closure with a
CPU-known group count. The indirect variant is identical except it takes an **args buffer + offset**
instead of `(groups_x, groups_y)` and records:

```rust
raw_body.cmd_dispatch_indirect(cmd, args_buffer, args_offset);
```

`ash::Device::cmd_dispatch_indirect` (core, reads a `VkDispatchIndirectCommand {x,y,z}` from the buffer) is
available on `self.device.raw()`. The pass declares the args buffer as `IndirectCommandRead` (from §1) in
its `accesses` list alongside the compute pass's storage reads/writes, so the graph derives the
producer→dispatch barrier. Signature mirrors `add_compute_pass` (same `pipeline`/`set`/`accesses`/`push`
plumbing), swapping the two group-count params for `(args_buffer: vk::Buffer, args_offset: vk::DeviceSize)`.

The **indirect draws** (`cmd_draw_indexed_indirect`, `cmd_draw_indexed_indirect_count`) need **no** new
render_graph.rs machinery — the `RgPass::…body(|cmd, _| { … })` model already records any command inside a
hand-written body (exactly how `add_compute_pass` records its dispatch and how `scene_pass.rs` records
`cmd_draw_indexed`). Confirm this phase only by writing a throwaway body that compiles a
`raw.cmd_draw_indexed_indirect(cmd, buf, 0, 1, stride)` and
`raw.cmd_draw_indexed_indirect_count(cmd, buf, 0, count_buf, 0, max, stride)` call (both core `ash::Device`
methods) — Phase 6 lands the real call sites in `scene_pass.rs::record_batch_submeshes`. No behaviour
change: the helper is defined and never called this phase.

### 4. Capability probes (`crates/rendering/src/device.rs`)

Mirror the mesh-shader probe. Today `probe_optional_features` (line 1047) reads core features + a chained
`PhysicalDeviceMeshShaderFeaturesEXT` and stores `mesh_shader_supported` on the `Capabilities` struct
(line 80). Add three fields to `Capabilities` and probe them in `probe_optional_features`:

- **`multi_draw_indirect`** — a core `VkPhysicalDeviceFeatures` bit: `core_features.multi_draw_indirect != 0`
  (`core_features` already read at line 1054). Lets Phase 6 batch many instances per
  `cmd_draw_indexed_indirect` (`drawCount > 1`).
- **`draw_indirect_count`** — `VK_KHR_draw_indirect_count`, core in Vulkan 1.2: read
  `PhysicalDeviceVulkan12Features::draw_indirect_count` via a chained `get_physical_device_features2`
  (the same pattern the RT/mesh probes use at lines 1070–1084). Lets Phase 6 use the `…_count` draw
  variant so the GPU-written draw count drives the draw with no CPU readback.
- **`acceleration_structure_indirect_build`** — `PhysicalDeviceAccelerationStructureFeaturesKHR::
  acceleration_structure_indirect_build`, probed in the **same** chained query the existing `rt_supported`
  block already builds (line 1068, `as_feat`) — read `as_feat.acceleration_structure_indirect_build != 0`
  and store it (only meaningful when `rt_supported`). **This flag decides Phase 7's build path** (fix
  #1/#15): `vkCmdBuildAccelerationStructuresIndirectKHR` when set, else the CPU worst-case /
  degenerate-pad build.

**Enable, not just probe** (modern-correct, avoids a second touch and a misleading "supported-but-unusable"
capability): in `create_logical_device` (line 1123), where the device feature chain is assembled (lines
1200–1231), enable each probed-supported feature guarded by its capability, mirroring how `enabled_core`
turns on `pipeline_statistics_query` / `fill_mode_non_solid` (lines 1187–1191) and how `ms_feat`/`as_feat`
are pushed when their extensions are present:

- `enabled_core = enabled_core.multi_draw_indirect(true)` when `core_features.multi_draw_indirect != 0`.
- `features12 = features12.draw_indirect_count(true)` when the device advertises it (add to the required
  `features12` builder at line 1203).
- `as_feat = as_feat.acceleration_structure_indirect_build(true)` when supported, inside the existing
  `enable_rt` guard (line 1226) — the AS feature struct is only pushed when RT is enabled.

Enabling an advertised optional feature is legal and adds no runtime cost or behaviour; it is the
difference between a probe that *reports* capability and one whose feature is actually usable when Phase 6/7
records the indirect draw / indirect AS build. On llvmpipe/lavapipe these probes report `false` and the
features stay off, exactly as `mesh_shader_supported` does — the degradation the existing device unit test
already asserts. Extend that test (or the `Capabilities`-default assertions) to confirm the three new
fields default `false` and never gate device selection.

### 5. Keyed `TransientResources::acquire` (`crates/rendering/src/transient.rs`) — fix #16

`TransientResources` (line 62) is today a **positional** grow-only pool: `acquire_buffer(frame, size,
usage)` (line 89) and `acquire_image(frame, desc)` (line 124) each advance a per-slot **cursor**
(`buffer_cursor` / `image_cursor`, lines 56–58), reused at the same acquire position every frame and
rewound in `begin_frame` (line 79). The cursor is order-fragile: it works only while every consumer
acquires in the identical order each frame. A conditionally-present tessellation pass (zero tessellated
instances some frames) — or the Phase-9 prism, a **second** transient consumer — would skip its acquire and
desync the cursor, handing the next consumer the wrong allocation. There are no consumers today, so it is
currently latent, but the whole planset turns `TransientResources` into its first (Phase 3) and second
(Phase 9) consumer, so the fragility must go **before** either lands.

Replace the positional acquire with a **keyed** variant — one acquire API only (NO-LEGACY, delete the
cursor path in the same change):

```rust
pub fn acquire_buffer(
    &mut self, frame: usize, key: &'static str, size: u64, usage: vk::BufferUsageFlags,
) -> crate::Result<vk::Buffer>
pub fn acquire_image(
    &mut self, frame: usize, key: &'static str, desc: &ImageDesc,
) -> crate::Result<(vk::Image, vk::ImageView)>
```

- Key on a **stable `&'static str` label** (e.g. `"tess-vb"`, `"tess-ib"`, `"tess-args"`, `"prism-aabb"`),
  matching the render graph's own `&'static str` pass-name convention (`RgPass::compute(name)`). A tiny
  linear-scan `Vec<(&'static str, TransientBuffer)>` (the key set is a handful) or a `HashMap` maps
  label → slot; the same label returns the same grow-only allocation every frame.
- `begin_frame` (line 79) no longer rewinds a cursor — with keyed slots there **is** no cursor to desync.
  A skipped consumer simply does not touch its key; every other key's slot is untouched and returns its
  same handle. This is strictly better than the positional API's "acquire in fixed order every frame,
  zero-size when skipped" workaround: order-independence is a property of the keyed map, not a discipline
  the caller must uphold.
- Keep the grow-only fit test unchanged (`capacity >= size && usage.contains(usage)` for buffers, exact
  `ImageDesc` equality for images) and the fence-safe recycle contract unchanged (a slot is only ever
  reused `MAX_FRAMES_IN_FLIGHT` frames later, after its fence, so an acquired transient outlives the GPU
  work reading it — the reason the pool exists). The `grow_bytes` doubling (line 30) and its test
  (line 152) are untouched.
- **Both** `acquire_buffer` and `acquire_image` become keyed. Leaving image acquire positional while buffer
  is keyed would re-introduce the exact desync the change removes; there are zero image consumers, so
  keying it too is free and is the consistent single-API result NO-LEGACY requires.

The Phase-3 **shrink/reclaim** path (bound session-peak VRAM under grow-only) builds on this keyed model —
a slot's running high-water is tracked per key, and a key not acquired for M frames is a natural zero-size
signal for reclaim — but the reclaim logic itself is Phase 3, not here. Phase 1 lands only the keyed
acquire with no shrink.

## Scope

- `saffron-rendering`: `render_graph.rs` (two `RgUsage` rows + table + two tests), `renderer.rs`
  (`add_indirect_compute_pass` helper), `device.rs` (`Capabilities` + probe + enable of three features),
  `transient.rs` (keyed acquire replacing positional). `resources.rs` unchanged (usage already flows
  through `Buffer::new`).
- No shader, no scene, no protocol, no editor, no `.smat`/mesh-format change. No new crate.

## Depends on

- Nothing. Phase 1 is one of the two independent foundations (with Phase 2); the DAG is **1→3, 2→3**.
  It touches only dormant / additive surface, so it can land before any other phase and keep the gate green.

## Verification

All CPU-only — no GPU required, which is the point of landing it first:

- **`cargo build --workspace` + `cargo clippy --workspace -- -D warnings`** clean (inside the
  `saffron-build` toolbox): the two enum rows, the helper, the probe fields, and the keyed acquire all
  compile with no warning; nothing calls the new surface, so no behaviour changes.
- **Graph rows unit-tested**: `usage_info_matches_the_golden_table` covers the two new rows exhaustively;
  the new hazard tests prove `StorageWriteCompute → IndexInputRead` and `StorageWriteCompute →
  IndirectCommandRead` each derive exactly one memory barrier with the right src/dst stage+access and no
  image barrier — the **no-op indirect pass round-trip** at the derivation layer (a producer write ordered
  ahead of an indirect-args read, no device needed).
- **Keyed-acquire test** (new, in `transient.rs`'s `tests` module beside `grow_bytes_doubles_and_never_shrinks`,
  line 152): acquire buffers under two keys for a frame; on the next frame **skip** one key and re-acquire
  the other, and assert the re-acquired key returns the **same** `vk::Buffer` handle and its grow-only slot
  is unchanged — proving a skipped consumer cannot desync any other slot (this is the whole reason for the
  keyed upgrade). A second case acquires a buffer whose usage includes `INDIRECT_BUFFER` and asserts a
  same-key re-acquire of equal size + usage reuses the allocation (the `INDIRECT_BUFFER` flow, §2). The
  pool's `new` is device-backed, so these tests run against a headless device via the existing
  test-support device harness (the same harness the other `saffron-rendering` device-touching tests use),
  or the pure `grow_bytes`/keying logic is factored to a device-free unit where possible.
- **Device-probe test**: the three new `Capabilities` fields default `false`, are never consulted during
  selection, and a software (llvmpipe) device is still created and used — extending the existing
  optional-feature-degradation assertion.
- **Milestone gate**: `just engine` then `just prepare-for-commit` (format + lint) clean. Because nothing
  is wired, a plain `just run` / `just run-engine-headless` renders byte-identically to before — the
  behaviour-preservation this phase promises. No GPU-with-eyes pass is needed for Phase 1 (the visual/RT
  crack-testing arrives with Phases 4–8, where geometry actually changes); the correct check here is that
  the reproducible gate stays green and the added surface is dead until a later phase lights it up.

## Risks

- **Enabling `drawIndirectCount` / `multiDrawIndirect` / `accelerationStructureIndirectBuild` at device
  creation is only legal when advertised.** Each enable must be guarded by its probe (mirroring the
  `ms_feat`/`as_feat` extension guards), or `create_device` fails validation on hardware/llvmpipe lacking
  the feature. The probe-then-guarded-enable pattern is exactly the existing mesh-shader/RT precedent, so
  the risk is a mechanical one (forgetting a guard), caught immediately by the software-device gate run.
- **`DRAW_INDIRECT` as the barrier stage for `cmd_dispatch_indirect`** — correct per spec
  (`VK_PIPELINE_STAGE_2_DRAW_INDIRECT_BIT` is the stage where indirect **draw *and* dispatch** parameters
  are consumed), but easy to second-guess. The golden-table row + hazard test pin it; validation layers on
  the first real indirect dispatch (Phase 3) confirm it on GPU.
- **Keyed acquire drops the fixed-order guarantee the positional API leaned on** — which is the intended
  upgrade, but it means a caller that re-uses the same label for two *different* logical buffers in one
  frame would collide on one slot. The mitigation is the discipline that labels are unique per logical
  transient (one label = one buffer), enforced by convention and made obvious by the small, explicit label
  set; the keyed test catches an accidental collision (two acquires of the same key in a frame return the
  same handle, which is a bug if they were meant to be distinct). No RT / watertightness surface is touched
  this phase (the barrier core is unchanged, so ordering-derived watertightness for the later tessellator
  is preserved by construction), so there is no geometry-correctness risk to carry here — the risks are
  confined to capability enablement and the acquire-key contract.
