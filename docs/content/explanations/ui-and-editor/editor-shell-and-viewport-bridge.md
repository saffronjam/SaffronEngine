+++
title = 'Editor shell and the viewport bridge'
weight = 1
+++

# Editor shell and the viewport bridge

The editor is a CEF (Chromium) application: a React/TypeScript front-end rendered through
**windowless OSR**, a purpose-built Rust shell (`editor/shell`) that owns a winit Wayland
toplevel, and the engine running as a separate process. The webview never renders the 3D
scene. The engine renders headless and the shell composites its frames below the transparent
UI ([viewport compositing](../viewport-compositing/)), so the viewport shows the live render
while the UI owns all chrome — including chrome blended over the scene itself. CEF paints the
UI off-screen (`on_paint`) and the shell uploads each frame to the toplevel surface at the
monitor's refresh.

Every editor operation that touches the scene rides the same JSON-over-unix-socket
[control protocol](../../tooling-and-control/control-plane-architecture/) the `sa` CLI
speaks. The engine workspace builds the `saffron-host` executable — a headless host
that boots the engine, publishes frames, and drains the control socket, with no panels of
its own.

## Two processes, one socket

The shell spawns `saffron-host` with `SAFFRON_EDITOR_NATIVE_VIEWPORT=1` (hidden window), a
per-instance `SAFFRON_CONTROL_SOCK` (pid-scoped, so two editor windows do not collide),
**two** shared-memory segment names — `SAFFRON_VIEWPORT_SHM_SCENE` and
`SAFFRON_VIEWPORT_SHM_ASSET`, one ring per [view](../viewport-compositing/) so each pane's
subsurface has frames even while parked. The engine is then the renderer and the webview is
the UI, talking only over that socket.

The TypeScript side is a typed client over one generic passthrough. Every scene, asset, and
render command is `invoke('control', { cmd, params })` — where `invoke` is the shell bridge's
`cefQuery` round-trip; the Rust layer forwards it verbatim, turns an engine `ok:false` into a
rejected promise, and otherwise resolves the result JSON. Adding a new `sa` command needs no
Rust change — the typed wrapper in `client.ts` and a DTO entry are all that move.

```ts
async function call<C extends keyof CommandResultMap>(
  cmd: C,
  params?: object,
): Promise<CommandResultMap[C]> {
  return invoke<CommandResultMap[C]>("control", { cmd, params: params ?? {} });
}
```

Rust handles only the lifecycle and presenter commands directly — `start_engine`,
`set_viewport_bounds(view, …)`, `set_viewport_parked(view, …)`, `quit_engine`, `engine_alive`
— because those manage the child process and the two compositor-side subsurfaces rather than
the scene.

> [!NOTE]
> The presenter is a Wayland subsurface and the UI is composited on winit's `wl_display`, so
> the editor requires a Wayland session.

## Auto-start and the loading overlay

On boot the shell spawns the engine (`auto_start`), installs the presenter worker
(`presenter::install`), then polls `viewport-native-info` with a child-liveness-aware bounded
retry that distinguishes "socket not bound yet" from "process crashed". React drives an
`engineStatus.phase` state machine — `idle → starting → attaching → ready` — and the
[viewport panel](../viewport-panel/) probes the same command before flipping to `ready`.
A `<LoadingOverlay/>` covers the viewport region until then; it paints an opaque
background, which also covers the transparent hole before the first frame arrives.

## Crash recovery

The reconcile poll doubles as a liveness watchdog: each tick it calls `engineAlive()`,
which uses `child.try_wait()` rather than a stale handle, so a dead engine reads as dead.
If the child has exited, the store flips `phase` back to `error`, the overlay reappears,
and it offers **Retry** (re-probe) and **Restart** (quit, re-spawn, re-probe).

## In the code

| What | File | Symbols |
|---|---|---|
| Typed passthrough client | `editor/src/control/client.ts` | `call`, `callRaw`, `client` |
| Lifecycle + presenter commands | `editor/src/control/client.ts` | `startEngine`, `setViewportBounds` (view), `setViewportParked` (view), `setActiveView`, `quitEngine`, `engineAlive` |
| Engine spawn + supervision | `editor/shell/src/engine.rs` | `spawn_engine`, `auto_start`, watchdog |
| App shell + lifecycle events | `editor/src/app/App.tsx` | `App`, `engine-phase` / `viewport-error` listeners |
| Phase state machine | `editor/src/state/store.ts` | `EngineStatus`, `setPhase` |
| Loading + crash overlay | `editor/src/app/LoadingOverlay.tsx` | `LoadingOverlay`, Retry / Restart |

## Related

- [Viewport compositing](../viewport-compositing/) — how the engine's frames reach the screen
- [Viewport panel](../viewport-panel/) — the host div the subsurface is glued to
- [Theme and fonts](../theme-and-fonts/) — the shadcn/Tailwind chrome around the viewport
- [Shared types](../../tooling-and-control/shared-types/) — the DTO-first wire contract the typed client consumes
