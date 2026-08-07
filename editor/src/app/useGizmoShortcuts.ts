/// Global viewport keyboard shortcuts: the gizmo operation, focus-on-selection, and deselect.
///
/// The engine renders windowless and receives no raw keyboard, so the webview owns these bindings —
/// it writes `store.gizmo` optimistically and fires `set-gizmo`, and the engine can never
/// double-fire with it. The handler is gated off while a text input is focused and while the
/// Settings modal is open (it holds focus on non-text elements, so the text-entry guard alone would
/// not catch it). Each shortcut matches the configured binding, so a rebind applies immediately.
import { useEffect } from "react";
import { client } from "../control/client";
import { useEditorStore } from "../state/store";
import { errorText, notify } from "../lib/flash";
import { matchesBinding, type CommandId } from "../lib/keybindings";
import type { GizmoState } from "../protocol";

type GizmoOp = GizmoState["op"];

const GIZMO_COMMANDS: { id: CommandId; op: GizmoOp }[] = [
  { id: "gizmo.translate", op: "translate" },
  { id: "gizmo.rotate", op: "rotate" },
  { id: "gizmo.scale", op: "scale" },
];

/// Log a rejected shortcut command. The hook is a global key listener with no panel
/// to anchor a flash banner, so the failure goes to the console rather than vanishing.
function logRejected(action: string, err: unknown): void {
  console.error(`${action} rejected:`, errorText(err));
}

/// True when the active element is a text-entry control, so shortcuts must not fire.
/// Shared with the undo/redo shortcut hook (one definition for the text-entry guard).
export function isTextEntryFocused(): boolean {
  const el = document.activeElement;
  if (!el || !(el instanceof HTMLElement)) {
    return false;
  }
  if (el.isContentEditable) {
    return true;
  }
  const tag = el.tagName;
  return tag === "INPUT" || tag === "TEXTAREA" || tag === "SELECT";
}

export function useGizmoShortcuts(): void {
  useEffect(() => {
    const onKeyDown = (event: KeyboardEvent): void => {
      if (isTextEntryFocused()) {
        return;
      }
      const store = useEditorStore.getState();
      // The settings modal owns the keyboard while open (its capture widget binds
      // keys); never drive the engine underneath it.
      if (store.settingsOpen) {
        return;
      }
      // Every shortcut here drives engine state, so only act once it is live.
      if (store.engineStatus.phase !== "ready") {
        return;
      }

      const overrides = store.keyBindings;

      // Play-mode family (not rebindable): Ctrl+P play/stop, Ctrl+Shift+P pause/resume,
      // Ctrl+Alt+P step. Webview-owned because Ctrl+P would open the print dialog
      // otherwise; the clicks are mirrored by the Topbar buttons. metaKey covers macOS.
      if ((event.ctrlKey || event.metaKey) && event.key.toLowerCase() === "p") {
        event.preventDefault();
        const ps = store.playState;
        if (event.altKey) {
          if (ps === "paused") {
            void client.step().catch((err: unknown) => logRejected("step", err));
          }
        } else if (event.shiftKey) {
          if (ps === "playing") {
            store.setPlayState("paused");
            void client.pause().catch((err: unknown) => {
              store.setPlayState("playing");
              logRejected("pause", err);
            });
          } else if (ps === "paused") {
            store.setPlayState("playing");
            void client.play().catch((err: unknown) => {
              store.setPlayState("paused");
              logRejected("play", err);
            });
          }
        } else if (ps === "edit") {
          store.setPlayState("playing");
          void client
            .play()
            .then((result) => {
              if (!result.hasPrimaryCamera) {
                notify("No primary camera — using the editor camera");
              }
            })
            .catch((err: unknown) => {
              store.setPlayState("edit");
              logRejected("play", err);
            });
        } else {
          store.setPlayState("edit");
          void client.stop().catch((err: unknown) => logRejected("stop", err));
        }
        return;
      }

      // Exact-modifier matching (a binding of "f" does not fire on Ctrl+F), so
      // unbound menu/OS chords still pass straight through. The gizmo is hidden
      // during play, so W/E/R only retarget it in edit mode.
      if (store.playState === "edit") {
        for (const { id, op } of GIZMO_COMMANDS) {
          if (matchesBinding(event, id, overrides)) {
            event.preventDefault();
            // Optimistic local update + the command; the reconcile poll's get-gizmo
            // read keeps it in sync with any external mutation (e.g. `sa set-gizmo`).
            store.setGizmo({ op });
            void client.setGizmo({ op }).catch((err: unknown) => logRejected("set-gizmo", err));
            return;
          }
        }
      }

      // Focus the editor camera on the current selection.
      if (matchesBinding(event, "camera.focus", overrides)) {
        const selectedId = store.selectedId;
        if (selectedId === null) {
          return;
        }
        event.preventDefault();
        void client.focus(selectedId).catch((err: unknown) => logRejected("focus", err));
        return;
      }

      // Deselect: clear local selection immediately and tell the engine; the
      // reconcile poll confirms via selectionVersion.
      if (matchesBinding(event, "selection.deselect", overrides)) {
        if (store.selectedId === null) {
          return;
        }
        event.preventDefault();
        store.setSelectedId(null);
        void client.deselect().catch((err: unknown) => logRejected("deselect", err));
        return;
      }
    };

    window.addEventListener("keydown", onKeyDown);
    return () => {
      window.removeEventListener("keydown", onKeyDown);
    };
  }, []);
}
