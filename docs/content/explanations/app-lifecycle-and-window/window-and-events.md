+++
title = 'Window and events'
weight = 4
+++

# Window and events

A window is the on-screen surface the renderer presents into and the source of the operating
system's input events. In Anima it is a thin facade over a
[winit](https://docs.rs/winit/latest/winit/) window plus six typed event signals. Input reaches
the rest of the program through those signals: a layer subscribes in `on_attach` and is called
back when the matching event arrives.

```rust
pub struct Window {
    handle: Option<WinitWindow>,   // None in headless mode
    width: u32,
    height: u32,
    should_close: bool,

    pub on_close: SubscriberList<()>,
    pub on_resize: SubscriberList<(u32, u32)>,           // width, height (pixels)
    pub on_key_pressed: SubscriberList<(KeyCode, bool)>, // keycode, is_repeat
    pub on_key_released: SubscriberList<KeyCode>,
    pub on_file_dropped: SubscriberList<PathBuf>,
    pub on_raw_event: SubscriberList<WindowEvent>,       // every raw winit event
}
```

Each signal is a `SubscriberList<Args>`, the engine-wide
[signal/slot type](../../core-and-conventions/signals-and-slots/): a handler returns `true` to
stop propagation or `false` to let later subscribers see the event. `KeyCode` is winit's
`PhysicalKey`, a location-stable physical key identity that downstream code matches exhaustively
(`PhysicalKey::Code(KeyCode::Escape)`).

## Two construction modes

`Window::new` builds a real OS window. winit 0.30 only creates windows from inside a running
event loop, so the constructor takes the host's `ActiveEventLoop` and does not own it.
`WindowConfig` supplies the title, the logical size, and visibility; the defaults are
`"Saffron"` at 1600×900, visible. The constructor reads the created window's `inner_size` back,
so a windowed `Window` starts at its real pixel size.

The windowed facade implements `HasWindowHandle` and `HasDisplayHandle` from
[raw-window-handle](https://docs.rs/raw-window-handle/latest/raw_window_handle/) by forwarding
to the winit window. [ash-window](https://docs.rs/ash-window/latest/ash_window/) in
`saffron-rendering` consumes that handle pair to create the Vulkan surface.

`Window::headless` builds the facade with no OS window behind it. The signals work normally,
the size starts at 0×0 until a `Resized` event arrives, and the handle accessors return
`HandleError::NotSupported` rather than a sentinel, so no Vulkan surface can be built on that
path. The headless editor host's `App` carries no window at all; `HostLayer::on_update`
constructs a headless `Window` per control drain as the stand-in the control plane's
`EngineContext` requires.

## Dispatch

The winit `ApplicationHandler` loop that receives OS events lives in `saffron-app` (see
[main loop](../main-loop-and-run/)); its `WindowedApp::window_event` hands every event to
`Window::dispatch_window_event`, the translation table. That method publishes the raw event to
`on_raw_event` first, then maps the events it recognizes:

| Signal | winit event | Payload |
|---|---|---|
| `on_close` | `WindowEvent::CloseRequested` | none; also latches `should_close` |
| `on_resize` | `WindowEvent::Resized` | new width, height in pixels |
| `on_key_pressed` | `WindowEvent::KeyboardInput` (pressed) | keycode, `is_repeat` |
| `on_key_released` | `WindowEvent::KeyboardInput` (released) | keycode |
| `on_file_dropped` | `WindowEvent::DroppedFile` | the dropped file's path |

`on_resize` publishes the physical pixel size: the handler reads winit's `PhysicalSize` and
updates `width`/`height` before publishing. `dispatch_window_event` needs no live event loop —
a synthesized `WindowEvent` is enough — so the translation is unit-tested headless. The
keyboard arm goes through `dispatch_key`, which takes the three fields the translation reads
(`physical_key`, `state`, `repeat`) because winit's `KeyEvent` carries a private field and
cannot be synthesized in a test.

## Closing

`CloseRequested` latches `should_close` and publishes `on_close`; `request_close` sets the same
latch programmatically. The windowed loop checks the latch in `about_to_wait` and exits, which
is how the control plane's `quit` command ends a standalone run:

```sh
sa quit   # → ctx.window.request_close() → should_close → the loop exits
```

The headless editor host has no OS window to close; it exits when its parent-death watch sees
the editor process vanish.

## Who subscribes

The exported-game binary is the main consumer: `PlayerLayer::wire_input` in `saffron-player`
routes window input into the shared `ScriptInputState` that Luau scripts read.

```rust
let input = Rc::clone(&self.input);
window.on_key_pressed.subscribe(move |(key, _repeat)| {
    if let Some(name) = key_name(key) {
        input.borrow_mut().held.insert(name);
    }
    false // later subscribers still see the key
});
```

Held keys come from the typed key signals. Mouse position, buttons, and scroll come from
`on_raw_event`, since no typed signal carries them; the raw sink fires before typed dispatch,
so it sees every event even when a typed handler later stops propagation. `Window` knows
nothing about these consumers — it publishes, and whoever subscribed is called.

The editor's viewport input takes a different route. The host runs headless, so fly-camera
look deltas and gizmo interaction arrive over the control plane rather than through window
signals.

## In the code

| What | File | Symbols |
|---|---|---|
| Window data + signals | `window/src/lib.rs` | `Window`, `WindowConfig` |
| Construction modes | `window/src/lib.rs` | `Window::new`, `Window::headless` |
| Event translation | `window/src/lib.rs` | `dispatch_window_event`, `dispatch_key` |
| Surface handles | `window/src/lib.rs` | `HasWindowHandle`, `HasDisplayHandle` impls |
| Event-loop driver | `app/src/lib.rs` | `WindowedApp::window_event`, `run_windowed` |
| Close latch consumers | `app/src/lib.rs`, `commands_asset.rs` | `WindowedApp::about_to_wait`, the `quit` registration |
| Game input wiring | `player/src/main.rs` | `PlayerLayer::wire_input`, `apply_mouse_event` |
| Signal primitive | `signal/src/lib.rs` | `SubscriberList`, `subscribe`, `publish` |

## Related

- [Main loop](../main-loop-and-run/) — the loop that feeds `dispatch_window_event` and checks `should_close`
- [Layers](../layer-system/) — the `on_attach` hook where subscriptions are made
- [Signals](../../core-and-conventions/signals-and-slots/) — the `SubscriberList` primitive and its dispatch contract
- [Signals reference](../../../reference/event-signals/) — the complete signal and accessor tables
