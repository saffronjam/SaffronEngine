/// The mouse-button command dispatcher. Mouse commands (tab back/forward, close hovered
/// tab) live in the keybinding registry as `mouse:<name>` bindings, so this routes a
/// pressed button to whichever command is bound to it. Two input paths feed it:
/// - The side buttons (GDK 8/9): WebKitGTK never hands them to the page, so the native
///   bridge intercepts them and re-emits a `mouse-button` event.
/// - The middle button: it reaches the DOM, so we catch it on `pointerdown` and suppress
///   the platform autoscroll/paste only when it actually triggers a command.
import { listen, type UnlistenFn } from "../shell";
import { useEffect } from "react";
import { mouseCommandFor, mouseToken, type MouseButtonName } from "../lib/keybindings";
import { useEditorStore } from "../state/store";

/// GDK button number from the native bridge → mouse token name.
function nativeName(button: number): MouseButtonName | null {
  if (button === 8) {
    return "back";
  }
  if (button === 9) {
    return "forward";
  }
  return null;
}

/// Run the command bound to `name`; returns true if a command consumed the press.
function dispatch(name: MouseButtonName): boolean {
  const store = useEditorStore.getState();
  // While the settings modal is open the capture listener (or nothing) owns the button.
  if (store.settingsOpen) {
    return false;
  }
  const command = mouseCommandFor(mouseToken(name), store.keyBindings);
  switch (command) {
    case "tab.navBack":
    case "tab.navForward": {
      const direction = command === "tab.navBack" ? -1 : 1;
      // Over the Assets panel the buttons drive its folder history instead of tabs.
      if (store.assetsPanelHovered && store.assetsFolderNav) {
        if (direction < 0) {
          store.assetsFolderNav.back();
        } else {
          store.assetsFolderNav.forward();
        }
        return true;
      }
      if (store.engineStatus.phase !== "ready") {
        return false;
      }
      store.navigateTabHistory(direction);
      return true;
    }
    case "tab.close": {
      if (store.hoveredTabId) {
        store.closeViewTab(store.hoveredTabId);
        return true;
      }
      return false;
    }
    default:
      return false;
  }
}

export function useMouseBindings(): void {
  useEffect(() => {
    let disposed = false;
    const unlisteners: UnlistenFn[] = [];
    const register = async (): Promise<void> => {
      const off = await listen<number>("mouse-button", (event) => {
        const name = nativeName(event.payload);
        if (name) {
          dispatch(name);
        }
      });
      if (disposed) {
        off();
        return;
      }
      unlisteners.push(off);
    };
    void register();

    const onPointerDown = (event: PointerEvent): void => {
      if (event.button === 1 && dispatch("middle")) {
        event.preventDefault();
      }
    };
    window.addEventListener("pointerdown", onPointerDown);

    return () => {
      disposed = true;
      window.removeEventListener("pointerdown", onPointerDown);
      for (const off of unlisteners) {
        off();
      }
    };
  }, []);
}
