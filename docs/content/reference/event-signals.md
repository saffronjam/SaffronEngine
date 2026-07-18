+++
title = 'Signals'
weight = 2
math = false
+++

# Signals

`saffron-signal` provides the single-threaded publish-and-subscribe primitive used throughout Anima. `saffron-window` exposes typed instances for window lifecycle and input events.

## Subscriber list

`SubscriberList<Args>` stores handlers of type `FnMut(Args) -> bool`. A `false` return continues dispatch; `true` stops propagation before later subscribers run. Every method takes `&self` through interior mutability.

| Member | Result |
|---|---|
| `SubscriberList::new()` | Creates an empty list. `Default` has the same result. |
| `subscribe(handler) -> SubscriptionId` | Appends a handler and returns its removal token. |
| `unsubscribe(id)` | Removes the matching handler. An unknown or inactive ID is a no-op. |
| `publish(args)` | Calls handlers in subscription order until one returns `true`. Requires `Args: Clone + 'static`. |
| `len()` | Returns the current handler count. |
| `is_empty()` | Reports whether the list has no handlers. |

`SubscriptionId(pub u64)` values start at `1`, increase for the lifetime of a list, and are not reused. The payload can be one value or a tuple such as `(u32, u32)`.

```rust
use saffron_signal::SubscriberList;

let resized = SubscriberList::<(u32, u32)>::new();
let subscription = resized.subscribe(|(width, height)| {
    println!("{width}x{height}");
    false
});

resized.publish((1280, 720));
resized.unsubscribe(subscription);
```

### Dispatch behavior

`publish` snapshots the subscription IDs before it begins and releases its internal borrow around each handler call. The resulting behavior is:

| Change during dispatch | Current publish | Next publish |
|---|---|---|
| Subscribe a handler | The new handler does not run. | The new handler runs in order. |
| Unsubscribe a later handler | The removed handler is skipped. | The removed handler stays absent. |
| Unsubscribe the active handler | The active call finishes. | The handler stays absent. |

Handlers and storage are not `Send`; publishers dispatch on the main thread.

## Window signals

`Window::dispatch_window_event` publishes `on_raw_event` first, then translates a [winit 0.30](https://docs.rs/winit/0.30/winit/) `WindowEvent` into the matching typed signal.

| Signal | Payload | Source event and effect |
|---|---|---|
| `on_raw_event` | `WindowEvent` | Every event, before typed dispatch. |
| `on_close` | `()` | `CloseRequested`; also sets the close latch. |
| `on_resize` | `(u32, u32)` | `Resized`; updates the stored pixel width and height first. |
| `on_key_pressed` | `(KeyCode, bool)` | A pressed `KeyboardInput`; the Boolean reports key repeat. |
| `on_key_released` | `KeyCode` | A released `KeyboardInput`. |
| `on_file_dropped` | `PathBuf` | `DroppedFile`. |

`KeyCode` aliases `winit::keyboard::PhysicalKey`, so subscribers receive the physical key position rather than a layout-dependent character.

## Window construction and state

| Member | Result |
|---|---|
| `Window::new(&ActiveEventLoop, &WindowConfig) -> Result<Window>` | Creates a resizable OS window. An OS creation failure is `Error::Create`. |
| `Window::headless()` | Creates a signal facade with no OS window and an initial size of `0x0`. |
| `is_windowed()` | Reports whether an OS window exists. |
| `width()` / `height()` | Return the current physical-pixel dimensions. |
| `should_close()` | Returns the close latch. |
| `request_close()` | Sets the close latch without publishing `on_close`. |
| `winit_window()` | Returns the underlying winit window in windowed mode, or `None` in headless mode. |

`WindowConfig` contains `title: String`, `width: u32`, `height: u32`, and `hidden: bool`. Its defaults are the literal title `"Saffron"`, `1600x900`, and `hidden: false`.

Headless windows still accept `dispatch_window_event`, which makes the translation path testable without an active event loop. Their raw window and display handle implementations return `HandleError::NotSupported`.

## Source map

| What | File | Symbols |
|---|---|---|
| Subscription storage and dispatch | `engine/crates/signal/src/lib.rs` | `SubscriberList`, `SubscriptionId` |
| Window signals and event translation | `engine/crates/window/src/lib.rs` | `Window`, `Window::dispatch_window_event`, `KeyCode` |
| Window configuration and construction | `engine/crates/window/src/lib.rs` | `WindowConfig`, `Window::new`, `Window::headless`, `Error`, `Result` |

## Related

- [Window and events](../../explanations/app-lifecycle-and-window/window-and-events/)
