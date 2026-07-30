+++
title = 'Reference'
weight = 30
bookCollapseSection = true
+++

# Reference

Reference pages catalogue the engine's exact surface: type signatures, data shapes, enum values, and the full control command list. Each page covers one crate boundary and is meant for lookup, not reading end to end.

## Pages

| Page | Covers |
|---|---|
| `core-types` | `Result<T>`, `Error`, `Ref<T>`, `Uuid`, `TimeSpan`, the log macros |
| `event-signals` | `SubscriberList`, `SubscriptionId`, the `Window` signals |
| `render-graph-api` | `RgUsage`, `RgPass`, `RgAttachment`, `RenderGraph::import_image`/`import_buffer`/`add_pass` |
| `renderer-api` | `Renderer::new`, the per-frame seam, `submit`, the feature toggles, `request_executor_mesh_pipeline` |
| `components` | every built-in component and its fields |
| `shader-descriptor-sets` | sets 0–7: bindless, lighting, instances, IBL + probes, screen-space, GI / RT |
| `control-commands` | every registered `sa` command, its params and output |
