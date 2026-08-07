//! The scoped session guard: the scene and registry are lent to a scripted call only while it is on
//! the stack.
//!
//! A `&mut Scene` lifetime cannot live inside the `'static` userdata an
//! [`crate::entity::EntityHandle`] becomes, so the borrow is re-supplied per call rather than cached
//! in the handle. The VM is single-threaded and `!Send`, so the scene is *moved into* a thread-local
//! slot for the call's duration and moved back out on scope exit. Nothing escapes into the VM, so
//! this needs no `unsafe`, and a handle kept past its session degrades to a logged no-op rather than
//! a dangling deref.
//!
//! Every accessor here returns `None` when no session is open; each caller resolves that to its
//! documented default.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use mlua::RegistryKey;

use saffron_core::Uuid;
use saffron_scene::{ComponentRegistry, Scene, ScriptInputState};

use crate::bridge::ScriptHostBridge;

thread_local! {
    /// `Some` exactly while a [`ScopedSession`] is alive. Moved in and out, so the caller cannot
    /// touch the scene for the call's duration.
    static SESSION: RefCell<Option<Scene>> = const { RefCell::new(None) };

    /// Read-only during a session, so it crosses as a shared `Arc` clone with nothing to move back
    /// out.
    static REGISTRY: RefCell<Option<Arc<ComponentRegistry>>> = const { RefCell::new(None) };

    /// `entity:destroy()` queues a uuid here and the handle stays valid for the rest of the handler;
    /// the runtime drains it after the instance loop, never mid-loop, because the loop iterates the
    /// instance vector by reference.
    static DEFERRED: RefCell<DeferredOps> = const { RefCell::new(DeferredOps::new()) };

    /// The host derives the input edges *before* the tick, so a session only reads this. It crosses
    /// by move like the scene, so there is no per-tick clone.
    static INPUT: RefCell<Option<ScriptInputState>> = const { RefCell::new(None) };

    /// The instance whose handler is running, so a queued message records its sender. `Uuid(0)`
    /// outside an instance handler.
    static SENDER: RefCell<Uuid> = const { RefCell::new(Uuid(0)) };

    /// Drained by the runtime after the instance loop. The payload rides as a registry ref so it
    /// survives the queue, and is released after dispatch.
    static MESSAGES: RefCell<Vec<ScriptMessage>> = const { RefCell::new(Vec::new()) };

    /// Read-only during a session, so it crosses as a shared `Rc` clone.
    static BRIDGE: RefCell<Option<Rc<dyn ScriptHostBridge>>> = const { RefCell::new(None) };
}

/// One queued inter-script message, dispatched after the instance loop.
pub struct ScriptMessage {
    /// The target instance, or `Uuid(0)` for a broadcast to every instance.
    pub target: Uuid,
    /// The sending instance (`Uuid(0)` when sent outside a handler).
    pub sender: Uuid,
    /// Invoked as `self:<handler>(sender, payload)`.
    pub handler: String,
    /// `None` when the payload was `nil`.
    pub payload: Option<RegistryKey>,
}

/// The structural ops a scripted call defers to its post-loop flush. Only `destroy` is queued;
/// `set_parent` and `spawn` run inline, because they touch components rather than the instance
/// vector.
#[derive(Default)]
pub struct DeferredOps {
    pub pending_destroy: Vec<Uuid>,
    /// Set whenever a queued op changes the hierarchy, so the flush runs `relink_hierarchy` exactly
    /// once.
    pub hierarchy_dirty: bool,
}

impl DeferredOps {
    const fn new() -> Self {
        Self {
            pending_destroy: Vec::new(),
            hierarchy_dirty: false,
        }
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.pending_destroy.is_empty() && !self.hierarchy_dirty
    }
}

/// Queues `uuid` for deferred destruction and marks the hierarchy dirty.
pub fn defer_destroy(uuid: Uuid) {
    if !session_active() {
        return;
    }
    DEFERRED.with(|slot| {
        let mut ops = slot.borrow_mut();
        ops.pending_destroy.push(uuid);
        ops.hierarchy_dirty = true;
    });
}

/// Takes and clears the deferred ops, for the runtime's post-loop flush.
pub fn take_deferred() -> DeferredOps {
    DEFERRED.with(|slot| std::mem::take(&mut *slot.borrow_mut()))
}

/// Runs `f` against the lent input snapshot.
pub fn with_input<R>(f: impl FnOnce(&ScriptInputState) -> R) -> Option<R> {
    INPUT.with(|slot| slot.borrow().as_ref().map(f))
}

/// Sets the instance whose handler is about to run, so a message queued from it records the right
/// sender. Cleared to `Uuid(0)` after the loop.
pub fn set_sender(uuid: Uuid) {
    SENDER.with(|slot| *slot.borrow_mut() = uuid);
}

/// The instance whose handler is running (`Uuid(0)` outside a handler), for the message sender and
/// the host log-sink tag.
#[must_use]
pub fn current_sender() -> Uuid {
    SENDER.with(|slot| *slot.borrow())
}

/// Queues an inter-script message, drained by the runtime after the instance loop.
pub fn queue_message(message: ScriptMessage) {
    if !session_active() {
        return;
    }
    MESSAGES.with(|slot| slot.borrow_mut().push(message));
}

/// Takes and clears the queued messages, for the runtime's post-loop dispatch.
pub fn take_messages() -> Vec<ScriptMessage> {
    MESSAGES.with(|slot| std::mem::take(&mut *slot.borrow_mut()))
}

/// Whether a scripted call is currently on the stack.
#[must_use]
pub fn session_active() -> bool {
    SESSION.with(|slot| slot.borrow().is_some())
}

/// Lends `bridge` to the active call so the physics-reaching bindings reach the host's callbacks.
/// The runtime sets it right after [`enter_session`]; [`ScopedSession`]'s drop clears it.
pub fn set_bridge(bridge: Rc<dyn ScriptHostBridge>) {
    if !session_active() {
        return;
    }
    BRIDGE.with(|slot| *slot.borrow_mut() = Some(bridge));
}

/// Runs `f` against the lent host-callback bridge. `None` also when the host installed none.
pub fn with_bridge<R>(f: impl FnOnce(&dyn ScriptHostBridge) -> R) -> Option<R> {
    BRIDGE.with(|slot| slot.borrow().as_ref().map(|b| f(b.as_ref())))
}

/// Opens a session, lending `scene` and `registry` to the thread-local slots.
///
/// The guard borrows `scene` for its whole lifetime, so the compiler enforces that the scene is lent
/// rather than aliased. Pass `input = None` for a call that runs without gameplay input
/// (`on_create`/`on_destroy`, the schema probe, the tests).
///
/// Panics on a re-entrant call rather than silently aliasing: the invariant is one call on the stack.
pub fn enter_session<'a>(
    scene: &'a mut Scene,
    registry: Arc<ComponentRegistry>,
    mut input: Option<&'a mut ScriptInputState>,
) -> ScopedSession<'a> {
    SESSION.with(|slot| {
        assert!(
            slot.borrow().is_none(),
            "script: a session is already active on this thread (re-entrant tick)"
        );
        let moved = std::mem::take(scene);
        *slot.borrow_mut() = Some(moved);
    });
    REGISTRY.with(|slot| {
        *slot.borrow_mut() = Some(registry);
    });
    INPUT.with(|slot| {
        *slot.borrow_mut() = input.as_deref_mut().map(std::mem::take);
    });
    DEFERRED.with(|slot| {
        *slot.borrow_mut() = DeferredOps::new();
    });
    MESSAGES.with(|slot| slot.borrow_mut().clear());
    SENDER.with(|slot| *slot.borrow_mut() = Uuid(0));
    BRIDGE.with(|slot| {
        slot.borrow_mut().take();
    });
    ScopedSession { scene, input }
}

/// Holds the caller's `&mut Scene` (and optional `&mut ScriptInputState`) and restores them on drop.
#[must_use = "the session ends when the guard is dropped; hold it for the call's duration"]
pub struct ScopedSession<'a> {
    scene: &'a mut Scene,
    input: Option<&'a mut ScriptInputState>,
}

impl Drop for ScopedSession<'_> {
    fn drop(&mut self) {
        SESSION.with(|slot| {
            if let Some(scene) = slot.borrow_mut().take() {
                *self.scene = scene;
            }
        });
        INPUT.with(|slot| {
            if let (Some(restored), Some(target)) = (slot.borrow_mut().take(), self.input.as_mut())
            {
                **target = restored;
            }
        });
        REGISTRY.with(|slot| {
            slot.borrow_mut().take();
        });
        DEFERRED.with(|slot| {
            *slot.borrow_mut() = DeferredOps::new();
        });
        MESSAGES.with(|slot| slot.borrow_mut().clear());
        SENDER.with(|slot| *slot.borrow_mut() = Uuid(0));
        BRIDGE.with(|slot| {
            slot.borrow_mut().take();
        });
    }
}

/// Runs `f` with a shared reference to the lent scene.
pub fn with_scene<R>(f: impl FnOnce(&Scene) -> R) -> Option<R> {
    SESSION.with(|slot| slot.borrow().as_ref().map(f))
}

/// The write counterpart of [`with_scene`].
pub fn with_scene_mut<R>(f: impl FnOnce(&mut Scene) -> R) -> Option<R> {
    SESSION.with(|slot| slot.borrow_mut().as_mut().map(f))
}

/// Runs `f` with the lent component registry, which drives the type-erased component bridge.
pub fn with_registry<R>(f: impl FnOnce(&ComponentRegistry) -> R) -> Option<R> {
    REGISTRY.with(|slot| slot.borrow().as_ref().map(|r| f(r)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use saffron_scene::register_builtin_components;

    fn registry() -> Arc<ComponentRegistry> {
        Arc::new(register_builtin_components())
    }

    #[test]
    fn no_session_open_by_default() {
        assert!(!session_active());
        assert!(with_scene(|_| ()).is_none());
        assert!(with_scene_mut(|_| ()).is_none());
        assert!(with_registry(|_| ()).is_none());
    }

    #[test]
    fn session_lends_scene_and_restores_on_drop() {
        let mut scene = Scene::new();
        let e = scene.create_entity("probe");
        {
            let _guard = enter_session(&mut scene, registry(), None);
            assert!(session_active());
            let found = with_scene(|s| s.valid(e)).expect("session open");
            assert!(found, "the lent scene carries the entity");
            let has_transform_row =
                with_registry(|r| r.find_by_name("Transform").is_some()).expect("registry lent");
            assert!(has_transform_row, "the lent registry resolves a row");
        }
        assert!(!session_active(), "the session closed on drop");
        assert!(
            with_registry(|_| ()).is_none(),
            "the registry was cleared on drop"
        );
        assert!(scene.valid(e), "the scene came back intact");
    }

    #[test]
    fn mutations_through_the_session_survive() {
        let mut scene = Scene::new();
        let e = scene.create_entity("probe");
        {
            let _guard = enter_session(&mut scene, registry(), None);
            with_scene_mut(|s| {
                s.add_component(
                    e,
                    saffron_scene::Name {
                        name: "renamed".to_owned(),
                    },
                )
                .expect("rename");
            })
            .expect("session open");
        }
        let name = scene
            .with_component::<saffron_scene::Name, _>(e, |n| n.name.clone())
            .expect("name present");
        assert_eq!(name, "renamed");
    }

    #[test]
    #[should_panic(expected = "already active")]
    fn re_entrant_session_panics() {
        let mut scene = Scene::new();
        let _outer = enter_session(&mut scene, registry(), None);
        let mut other = Scene::new();
        let _inner = enter_session(&mut other, registry(), None);
    }
}
