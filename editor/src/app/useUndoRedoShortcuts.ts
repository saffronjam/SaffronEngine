/// Global undo/redo shortcuts dispatched to the ACTIVE main tab's history. Gated like the gizmo
/// shortcuts: off while a text field is focused (the browser keeps its own text undo there), while
/// the settings modal is open, and until the engine is ready. The mouse Back/Forward path shares
/// these store actions but is deliberately not text-gated.
import { useEffect } from "react";
import { useEditorStore } from "../state/store";
import { matchesBinding } from "../lib/keybindings";
import { isTextEntryFocused } from "./useGizmoShortcuts";

export function useUndoRedoShortcuts(): void {
  useEffect(() => {
    const onKeyDown = (event: KeyboardEvent): void => {
      const store = useEditorStore.getState();
      if (store.settingsOpen || store.engineStatus.phase !== "ready") {
        return;
      }
      // Alt+Left / Alt+Right is the webview's Back/Forward navigation; trap it (an SPA
      // reload would lose all editor state) and route it to undo/redo, like the mouse
      // side buttons. Not text-gated — it is a navigation chord, not text entry.
      if (
        event.altKey &&
        !event.ctrlKey &&
        !event.metaKey &&
        (event.key === "ArrowLeft" || event.key === "ArrowRight")
      ) {
        event.preventDefault();
        if (event.key === "ArrowLeft") {
          void store.undo();
        } else {
          void store.redo();
        }
        return;
      }
      // Ctrl+Z / Ctrl+Shift+Z / Ctrl+Y: text-gated so the browser keeps its own text
      // undo while an input is focused.
      if (isTextEntryFocused()) {
        return;
      }
      const overrides = store.keyBindings;
      if (matchesBinding(event, "edit.undo", overrides)) {
        event.preventDefault();
        void store.undo();
        return;
      }
      // The bound redo chord, plus the fixed Ctrl+Y alias (not rebindable).
      const ctrlY =
        (event.ctrlKey || event.metaKey) &&
        !event.shiftKey &&
        !event.altKey &&
        event.key.toLowerCase() === "y";
      if (matchesBinding(event, "edit.redo", overrides) || ctrlY) {
        event.preventDefault();
        void store.redo();
      }
    };
    window.addEventListener("keydown", onKeyDown);
    return () => {
      window.removeEventListener("keydown", onKeyDown);
    };
  }, []);
}
