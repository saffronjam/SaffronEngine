+++
title = 'Tooling & control'
weight = 15
bookCollapseSection = true
+++

# Tooling & control

The control plane is a JSON-over-unix-socket protocol that drives a running editor from outside the process. The socket is non-blocking and drained once per frame on the main thread, so commands apply between frames without stalling the render loop. Through it the `sa` CLI creates entities, sets components, imports assets, toggles render features, and grabs screenshots. Each engine feature ships a matching command, which keeps the editor scriptable and visually debuggable from a shell.

## Pages

| Page | Covers | Code |
|---|---|---|
| `control-plane-architecture` | the socket, typed `CommandRegistry::register`, the `EngineContext` borrow seam, per-frame drain | `engine/crates/control` |
| `sa-cli-protocol` | the Rust `sa` bin: JSON request/response shape, `clap` surface, token coercion, the shared wire client | `engine/crates/sa`; `engine/crates/control-client` |
| `scene-commands` | list/create/destroy/select, parent, set component(-field), transform, material, light, camera, gizmo, pick, focus, inspect | `engine/crates/control/src/commands_scene.rs` |
| `render-commands` | set-aa / set-clustered / set-ibl / set-ssao / set-ssgi / set-shadows / set-gi / set-exposure / set-depth-prepass, render-stats | `engine/crates/control/src/commands_render.rs` |
| `asset-commands` | import-model/texture, instantiate-model, catalog + folders, assign-asset, thumbnails, save/load project | `engine/crates/control/src/commands_asset.rs` |
| `project-loading` | non-blocking project bring-up: phases + boot stages, the off-thread doc worker, the busy gate, `project-status` / `cancel-load` | `engine/crates/control/src/project_loader.rs`; `engine/crates/assets/src/project_load.rs` |
| `screenshots-and-capture` | viewport vs. window PNG, deferred swapchain capture | `engine/crates/control/src/commands_asset.rs`; `engine/crates/rendering/src/renderer.rs` |
| `shared-types` | DTO-first wire contract: Rust DTOs → serde / OpenRPC / TS / manifest via `xtask gen-protocol`, the freshness gate, wire invariants | `engine/crates/protocol`; `engine/xtask/src/protocol` |
