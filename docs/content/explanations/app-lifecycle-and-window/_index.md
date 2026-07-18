+++
title = 'App lifecycle & window'
weight = 2
bookCollapseSection = true
+++

# App lifecycle & window

The application lifecycle is the path an Anima host follows from start to shutdown:
the `run` loop, the layers a client attaches, and the winit window with its typed event
signals. Every feature hangs off a layer hook.

## Pages

| Page | Covers | Code |
|---|---|---|
| [main-loop-and-run](main-loop-and-run/) | `AppConfig`, `run`, the per-frame sequence, reactive pacing (idle when static), windowed vs headless modes, `on_create`/`on_exit` | `app/src/lib.rs` · `run`, `step_frame`, `RedrawController` |
| [layer-system](layer-system/) | `Layer` as a trait of default-empty hooks, `attach_layer`, the hook set | `app/src/lib.rs` · `Layer`, `attach_layer`, `run_hook` |
| [the-submit-and-rendergraph-seams](the-submit-and-rendergraph-seams/) | `on_render` submit seam vs. `on_render_graph` pass authoring | `rendering/src/renderer.rs` · `submit`; `rendering/src/render_graph.rs` · `add_pass` |
| [window-and-events](window-and-events/) | winit window, typed signals (`on_resize`/`on_key_pressed`/…), raw event sink | `window/src/lib.rs` · `Window`, `dispatch_window_event` |
| [headless-and-capture](headless-and-capture/) | `SAFFRON_EXIT_AFTER_FRAMES`, reactive pacing, headless mode, control-plane capture | `app/src/lib.rs` · `frame_limit_from_env`, `pace_iteration` |
