import { useEffect, type RefObject } from "react";
import { client } from "../../control/client";
import { bindingFor } from "../../lib/keybindings";
import { useEditorStore } from "../../state/store";
import { getCurrentWindow, listen } from "../../shell";
import { FLY_STREAM_MS } from "./viewportInput";

/// RMB fly-cam: hold RMB over the viewport to fly. CEF's windowless OSR can't do DOM pointer lock, so
/// the shell locks the cursor natively (`setPointerLock`) and streams relative look motion back as
/// `fly-look` events; the WASD/Space/Shift key state + accumulated look stream to the engine over
/// `fly-input`. Release RMB or press Esc (or lose focus) to end. `flyingRef` is shared with the
/// pointer-interaction effect, which stands down while the fly owns the locked cursor.
export function useFlyCamera(
  hostRef: RefObject<HTMLDivElement | null>,
  flyingRef: RefObject<boolean>,
): void {
  useEffect(() => {
    const el = hostRef.current;
    if (!el) {
      return;
    }
    const appWindow = getCurrentWindow();

    const keys = { forward: false, back: false, left: false, right: false, up: false, down: false };
    let lookDx = 0;
    let lookDy = 0;
    let sendTimer: ReturnType<typeof setTimeout> | null = null;
    let flyLookUnlisten: (() => void) | null = null;
    let disposed = false;

    const sendState = (active: boolean): void => {
      const dx = lookDx;
      const dy = lookDy;
      lookDx = 0;
      lookDy = 0;
      void client.flyInput({ active, lookDx: dx, lookDy: dy, ...keys }).catch(() => {});
    };

    const scheduleSend = (): void => {
      if (sendTimer !== null) {
        return;
      }
      sendTimer = setTimeout(() => {
        sendTimer = null;
        if (flyingRef.current) {
          sendState(true);
        }
      }, FLY_STREAM_MS);
    };

    const endFly = (): void => {
      if (!flyingRef.current) {
        return;
      }
      flyingRef.current = false;
      if (sendTimer !== null) {
        clearTimeout(sendTimer);
        sendTimer = null;
      }
      for (const key of Object.keys(keys) as (keyof typeof keys)[]) {
        keys[key] = false;
      }
      lookDx = 0;
      lookDy = 0;
      void appWindow.setPointerLock(false).catch(() => {});
      sendState(false);
    };

    const onPointerDown = (event: PointerEvent): void => {
      if (event.button !== 2 || flyingRef.current) {
        return;
      }
      event.preventDefault();
      flyingRef.current = true;
      void appWindow.setPointerLock(true).catch(() => {});
      sendState(true);
    };

    const onPointerUp = (event: PointerEvent): void => {
      if (event.button === 2) {
        endFly();
      }
    };

    // Relative look motion arrives from the shell's locked cursor (`DeviceEvent::MouseMotion`), not the
    // DOM — CEF OSR delivers no mouse moves while the pointer is grabbed.
    void listen<{ dx: number; dy: number }>("fly-look", (event) => {
      if (!flyingRef.current) {
        return;
      }
      lookDx += event.payload.dx;
      lookDy += event.payload.dy;
      scheduleSend();
    }).then((fn) => {
      if (disposed) {
        fn();
      } else {
        flyLookUnlisten = fn;
      }
    });

    // Map a physical key code to a fly direction via the configured (hold-kind)
    // bindings. Read live from the store so a rebind in settings applies without
    // re-running this effect.
    const keyFor = (code: string): keyof typeof keys | null => {
      const overrides = useEditorStore.getState().keyBindings;
      if (code === bindingFor("camera.flyForward", overrides)) {
        return "forward";
      }
      if (code === bindingFor("camera.flyBack", overrides)) {
        return "back";
      }
      if (code === bindingFor("camera.flyLeft", overrides)) {
        return "left";
      }
      if (code === bindingFor("camera.flyRight", overrides)) {
        return "right";
      }
      if (code === bindingFor("camera.flyUp", overrides)) {
        return "up";
      }
      if (code === bindingFor("camera.flyDown", overrides)) {
        return "down";
      }
      return null;
    };

    const onKey =
      (down: boolean) =>
      (event: KeyboardEvent): void => {
        if (!flyingRef.current) {
          return;
        }
        // Esc ends the fly (replacing the native pointer-lock exit).
        if (down && event.code === "Escape") {
          event.preventDefault();
          endFly();
          return;
        }
        const key = keyFor(event.code);
        if (!key) {
          return;
        }
        event.preventDefault();
        if (keys[key] !== down) {
          keys[key] = down;
          scheduleSend();
        }
      };

    const onContextMenu = (event: Event): void => event.preventDefault();

    const keyDown = onKey(true);
    const keyUp = onKey(false);
    el.addEventListener("pointerdown", onPointerDown);
    window.addEventListener("pointerup", onPointerUp);
    window.addEventListener("keydown", keyDown);
    window.addEventListener("keyup", keyUp);
    // Losing focus mid-fly would otherwise strand held keys and the locked cursor.
    window.addEventListener("blur", endFly);
    el.addEventListener("contextmenu", onContextMenu);

    return () => {
      disposed = true;
      endFly();
      flyLookUnlisten?.();
      el.removeEventListener("pointerdown", onPointerDown);
      window.removeEventListener("pointerup", onPointerUp);
      window.removeEventListener("keydown", keyDown);
      window.removeEventListener("keyup", keyUp);
      window.removeEventListener("blur", endFly);
      el.removeEventListener("contextmenu", onContextMenu);
    };
  }, [hostRef, flyingRef]);
}
