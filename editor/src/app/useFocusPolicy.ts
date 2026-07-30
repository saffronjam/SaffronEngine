/// Native-app focus policy: traps Tab so it never walks a focus ring through the editor chrome,
/// where Enter/Space on a stray-focused toolbar button would fire it by accident. Tab is left alone
/// inside an open dialog, which Radix already focus-traps.
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
