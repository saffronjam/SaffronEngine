+++
title = 'Layers'
weight = 2
+++

# Layers

A layer is a unit of program behavior the app runs at each phase of the frame. It is a trait
with provided, default-empty methods, not a base class with required overrides. The app keeps a
list of boxed layers and invokes each hook on every one; a hook the layer does not override
falls through to the empty default.

```rust
pub trait Layer {
    fn name(&self) -> &str { "Layer" }
    fn on_attach(&mut self, _app: &mut App) {}
    fn on_update(&mut self, _app: &mut App, _dt: TimeSpan) {}
    fn on_render(&mut self, _app: &mut App) {}                          // submit GPU work
    fn on_ui(&mut self, _app: &mut App) {}
    fn on_render_graph(&mut self, _app: &mut App, _graph: &mut RenderGraph) {}  // add passes
    fn on_detach(&mut self, _app: &mut App) {}
}
```

## How dispatch works

A client implements `Layer` on its own type, overrides the hooks it needs, and pushes a boxed
instance with `attach_layer`, usually from `AppConfig::on_create`. `App` stores the layers in a
flat `Vec<Box<dyn Layer>>` in attach order. `Box<dyn Layer>` is a
[trait object](https://doc.rust-lang.org/book/ch18-02-trait-objects.html), so the layer set
stays open: any client type qualifies by implementing the trait.

At each phase of the frame, the loop walks the vec and calls that phase's hook on every layer
through `run_hook`:

```rust
run_hook(app, |layer, app| layer.on_update(app, dt));
```

`run_hook` moves the layer vec out of `App` for the duration of the pass with a `mem::take`,
so a hook can borrow `&mut App` (the window, the frame host, the `running` latch) without
aliasing the list being iterated. After the pass it restores the vec, appending any layers a hook
attached mid-pass. Each hook takes `&mut App` as a parameter rather than capturing it, so a
layer never aliases the app it runs inside.

## The hook set

Each hook maps to a fixed point in [the main loop](../main-loop-and-run/):

| Hook | When it runs | Typical use |
|---|---|---|
| `on_attach` | once, after `on_create`, before the first frame | allocate resources, subscribe to window signals |
| `on_update(dt)` | every loop iteration, first | game logic, camera, animation; `dt` is a wall-clock `TimeSpan` |
| `on_render` | rendered frames, after `begin_frame` | record GPU work through the [submit seam](../the-submit-and-rendergraph-seams/) |
| `on_ui` | rendered frames, after `on_render` | UI and overlay geometry (the host renders the scene here) |
| `on_render_graph(graph)` | rendered frames, after `on_ui` | add passes to the frame's render graph |
| `on_detach` | once at teardown, after `wait_gpu_idle`, before `on_exit` | drop GPU resources, join workers |

Only `on_update` is guaranteed every iteration. The loop renders reactively: when the
`RedrawController` verdict is idle, the viewport is minimized, or `begin_frame` declines the
frame (an out-of-date swapchain), the three render hooks are skipped and the last published
frame stays on screen. `on_update` still fires on those iterations, so control-socket draining
and game logic keep running while the GPU is quiet.

The two rendering hooks split by intent. `on_render` records commands into the current frame;
`on_render_graph` is handed the frame's `RenderGraph` so the layer can add whole passes. That
split is the subject of [the submit and render-graph seams](../the-submit-and-rendergraph-seams/).

A run with `SAFFRON_EXIT_AFTER_FRAMES=3` and one layer produces exactly this hook trace,
asserted by the loop's unit tests on a GPU-free `FrameHost`:

```
attach
update  render  ui  render_graph      # frame 1
update  render  ui  render_graph      # frame 2
update  render  ui  render_graph      # frame 3
detach
```

## Why a trait of hooks

A trait with provided methods keeps every hook independently optional: an empty layer
(`impl Layer for Empty {}`) compiles with no override boilerplate. A layer carries its own
state in the implementing type's fields, so the same contract fits a small test probe and the
full editor host. The price is one dynamic dispatch per hook per layer per phase, negligible
next to a frame's GPU work.

The editor host is a single layer, `HostLayer` in `saffron-host`, whose fields hold the editor
session, the asset server, the control plane, and the play-mode runtime:

- `on_attach` bootstraps the project and requests the first redraw.
- `on_update` drains the control socket, advances the session, and sets the reactive-render
  activity for the frame.
- `on_ui` renders the scene and submits the gizmo overlay.
- `on_detach` sequences the teardown (worker join, socket close, GPU-cache clear) before the
  renderer drops.

## In the code

| What | File | Symbols |
|---|---|---|
| Layer trait | `app/src/lib.rs` | `Layer` |
| Attaching | `app/src/lib.rs` | `attach_layer`, `App` |
| Dispatch | `app/src/lib.rs` | `run_hook`, `step_frame`, `run_frame`, `start`, `finish` |
| The host layer | `host/src/layer.rs` | `HostLayer` |

> [!TIP]
> Layers run in attach order at every phase, and there is no priority or removal API. If one
> layer's `on_update` must see the result of another's, attach it second. A layer attached from
> inside a hook joins on the *next* pass, not the current one — and the loop never replays the
> attach pass, so only layers attached in `on_create` receive `on_attach`.

## Related

- [Main loop](../main-loop-and-run/) — where the hooks are invoked
- [Render seams](../the-submit-and-rendergraph-seams/) — what `on_render` vs `on_render_graph` do
- [Window and events](../window-and-events/) — the signals a layer subscribes to in `on_attach`
