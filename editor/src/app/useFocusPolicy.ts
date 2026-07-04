/// Native-app focus policy. The web platform makes every control Tab-focusable and walks
/// a focus ring through the DOM on each Tab — behavior that does not belong in a game
/// editor, where Enter/Space on a stray-focused control (a toolbar button) fires it by
/// accident. This traps the Tab key so it never moves focus through the editor chrome.
///
/// Tab is left alone inside an open dialog (`[role="dialog"]`): Radix already focus-traps
/// its modals, so field/button navigation there stays fully functional. Tree/list
/// keyboard navigation (arrow keys on a click-focused row) is a deliberate future
/// addition and needs no Tab traversal.
import { useEffect } from "react";

/// True while focus sits inside an open modal, where native Tab navigation is expected.
function tabAllowed(): boolean {
  const el = document.activeElement;
  return el instanceof HTMLElement && el.closest('[role="dialog"]') !== null;
}

export function useFocusPolicy(): void {
  useEffect(() => {
    const onKeyDown = (event: KeyboardEvent): void => {
      if (event.key !== "Tab" || tabAllowed()) {
        return;
      }
      // Prevent the focus move only; propagation is untouched, so a command bound to Tab
      // (or the settings capture widget) still receives the event.
      event.preventDefault();
    };
    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, []);
}
