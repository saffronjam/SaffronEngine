+++
title = 'Tooling & control'
weight = 15
bookCollapseSection = true
+++

# Tooling & control

The control plane gives the editor, the `sa` CLI, and test drivers one typed command surface over a
running host. Requests travel as newline-delimited JSON over a local Unix socket. The host drains the
socket on the main thread between frames, so handlers can borrow the live scene, renderer, asset
server, window, and play-mode physics world without shared-state locks.

Each request names a command and carries a parameter object. The reply echoes the request ID and
contains either a result or a typed error:

```json
{"id":7,"cmd":"ping","params":{}}
{"id":7,"ok":true,"result":{"pong":true,"engine":"Saffron Anima","version":"0.1.0-vulkan","pid":4312}}
```

`CommandRegistry::register<P, R>` deserializes the params DTO, invokes the handler, and serializes
the result DTO. Those DTOs live in `saffron-protocol`; the protocol generator derives TypeScript,
OpenRPC, the command manifest, and Luau definitions from the same Rust types. Registry tests compare
the live command set with that manifest.

The socket is non-blocking, but ordinary handlers are synchronous and must finish within the frame.
Project loading uses a bounded worker for filesystem parsing and catalog preparation, then installs
the prepared state on the main thread. While a load is active, the dispatcher admits only the small
set of commands needed for liveness, progress, cancellation, and viewport readiness.

## Pages

| Page | Covers | Code |
|---|---|---|
| [`control-plane-architecture`](control-plane-architecture/) | Socket framing, dispatch, live borrows, and redraw classification | `ControlServer`, `ControlContext`, `CommandRegistry`, `EngineContext` |
| [`sa-cli-protocol`](sa-cli-protocol/) | CLI requests, token coercion, output, and host launch | `sa`, `saffron-control-client`, `fold_positional_args` |
| [`scene-commands`](scene-commands/) | Entities, components, selection, cameras, gizmos, and scripts | `register_scene_commands` |
| [`render-commands`](render-commands/) | Render settings, statistics, profiling, probes, and view modes | `register_render_commands` |
| [`asset-commands`](asset-commands/) | Catalog, import, previews, materials, projects, and sessions | `register_asset_commands` |
| [`screenshots-and-capture`](screenshots-and-capture/) | Viewport and window screenshots at safe frame boundaries | `capture_viewport`, `request_window_capture` |
| [`shared-types`](shared-types/) | DTOs, wire invariants, schemas, manifests, and generated clients | `COMMANDS`, `gen-protocol`, `WireUuid` |
| [`project-loading`](project-loading/) | Worker preparation, main-thread installation, progress, and cancellation | `ProjectLoader`, `ProjectDocWorker`, `project-status` |

## In the code

| What | File | Symbols |
|---|---|---|
| Socket server | `control/src/server.rs` | `ControlServer`, `start_control_server`, `control_socket_path` |
| Per-frame orchestration | `control/src/context.rs` | `ControlContext::poll`, `advance_project_load` |
| Typed dispatch | `control/src/registry.rs` | `CommandRegistry`, `register_builtin_commands`, `EngineContext` |
| Wire DTOs | `protocol/src/dto.rs` | command parameter and result types |
| Command inventory | `protocol/src/command.rs` | `COMMANDS`, `CommandSpec` |
| Protocol generator | `xtask/src/protocol/` | `emit`, `emit_envelope_schema`, `emit_openrpc`, `emit_manifest` |
