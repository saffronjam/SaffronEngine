+++
title = 'Signals'
weight = 6
+++

# Signals

A signal is a typed broadcast channel: a producer publishes an event, and every subscribed handler
receives it in turn. This is the [signals-and-slots](https://doc.qt.io/qt-6/signalsandslots.html)
pattern — the producer knows only the payload type, never who listens. Anima expresses it with one
type, `SubscriberList<Args>` in `saffron-signal`. A handler can also stop an event from reaching
later subscribers, so each list doubles as a prioritized chain.

## The shape

```rust
pub struct SubscriberList<Args> {
    entries: RefCell<Vec<Entry<Args>>>,
    next_id: Cell<u64>,
}

impl<Args> SubscriberList<Args> {
    pub fn subscribe(&self, handler: impl FnMut(Args) -> bool + 'static) -> SubscriptionId;
    pub fn unsubscribe(&self, id: SubscriptionId);
}

impl<Args: Clone + 'static> SubscriberList<Args> {
    pub fn publish(&self, args: Args);
}
```

`Args` is the payload, fixed at the type. A single value carries one thing; a tuple carries
several — `SubscriberList<(u32, u32)>` carries a resize's width and height. `publish` requires
`Args: Clone` because each subscriber receives its own copy.

The list is single-thread (`!Send`): every consumer dispatches on the main thread, so there is no
lock around the subscriber set. The entries sit behind `RefCell`/`Cell` interior mutability and
every method takes `&self`, which is what lets a handler reach the same list to subscribe or
unsubscribe, itself included, while a `publish` is in flight.

## Subscription tokens

`subscribe` boxes the handler, stores it under a monotonically increasing id, and returns a
`SubscriptionId`, a thin `u64` newtype. The caller keeps the token and passes it to `unsubscribe`
later; removing an id that is already gone is a no-op. Ids are never reused for the lifetime of the
list, so a stale token cannot match a newer subscription.

`len` and `is_empty` report the live handler count. The host's `play_hooks_live` check reads
`is_empty` to assert its lifecycle subscriptions are installed, and its teardown test asserts an
empty list after detach.

## Stop-propagation dispatch

A handler returns `bool` meaning "stop here". `publish` walks the subscribers in subscription
order and breaks the moment one returns `true`, so earlier subscribers outrank later ones.
Claiming an event is a visible statement in the handler body: return `true` and no later handler
sees that keystroke or click. There is no hidden `consumed` flag mutated elsewhere.

## Snapshot iteration

A handler may subscribe or unsubscribe *during* dispatch, including unsubscribing itself.
`publish` makes that safe by iterating a snapshot of the subscriber ids taken at entry: an id
removed mid-dispatch is skipped, and one added mid-dispatch does not fire until the next publish.
The set of handlers for a publish is fixed the moment it starts, which removes a whole class of
reentrancy bug for the price of one id vector per publish.

Per id, `publish` takes the handler out of its slot under a short `entries` borrow, swapping in a
no-op closure, and invokes it with no borrow held — so the handler body can re-enter `subscribe`
or `unsubscribe` without aliasing a live borrow. Afterwards the handler is restored if its entry
still exists; a handler that unsubscribed itself has no slot left, and the box drops there. An id
a prior handler removed yields no handler to take and is skipped.

## Where it is used

The [window](../../app-lifecycle-and-window/window-and-events/) holds the widest-used lists.
`dispatch_window_event` publishes each winit event raw on `on_raw_event`, then translates it into
the typed signals: `on_close`, `on_resize` `(width, height)`, `on_key_pressed`
`(KeyCode, is_repeat)`, `on_key_released`, and `on_file_dropped`. The `saffron-player` binary's
`wire_input` subscribes the key signals and `on_raw_event` to maintain the held-key set and mouse
state that scripts read through `ScriptInputState`.

The editor session state in `saffron-sceneedit` publishes two signals of its own. `set_selection`
fires `on_selection_changed` with the newly selected `Entity` after bumping `selection_version`,
and every play transition fires `on_play_state_changed` with the new `PlayState`. The host
subscribes marker handlers on the play-state signal at attach (`install_play_state_hooks`) and
unsubscribes them by token on detach, so a dangling subscription is a detectable teardown bug.

> [!NOTE]
> A subscribed closure runs while the publisher may still hold a `&mut` borrow of the surrounding
> state — `publish_transition` is `&mut self` on the editor context, so a play-state handler
> cannot reach the play scene it would need to build a script VM or physics world. The host
> instead detects the Edit↔Playing edge itself in `reconcile_play_edge`, after that borrow
> releases; the subscriptions stay as lifecycle markers.

## In the code

| What | File | Symbols |
|---|---|---|
| The primitive | `engine/crates/signal/src/lib.rs` | `SubscriberList`, `SubscriptionId` |
| Register / remove / count | `engine/crates/signal/src/lib.rs` | `subscribe`, `unsubscribe`, `len`, `is_empty` |
| Snapshot + stop-propagation dispatch | `engine/crates/signal/src/lib.rs` | `publish` |
| Typed window signals | `engine/crates/window/src/lib.rs` | `dispatch_window_event`, `on_resize`, `on_raw_event` |
| Editor signals | `engine/crates/sceneedit/src/context.rs`, `play.rs` | `on_selection_changed`, `set_selection`, `on_play_state_changed`, `publish_transition` |
| Host lifecycle markers | `engine/crates/host/src/layer.rs` | `install_play_state_hooks`, `play_hooks_live` |
| Player input wiring | `engine/crates/player/src/main.rs` | `wire_input` |

## Related

- [Window and events](../../app-lifecycle-and-window/window-and-events/) — the typed signals built on this
- [Go-flavored design](../go-flavored-design/) — the API ethos the primitive follows
