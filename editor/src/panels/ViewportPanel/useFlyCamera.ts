import { useEffect, type RefObject } from "react";
import { bindingFor } from "../../lib/keybindings";
import { useEditorStore } from "../../state/store";
import { invoke, listen } from "../../shell";

/// RMB fly-cam: hold RMB over the viewport to fly. The shell owns the whole input path — it locks
/// the cursor natively (CEF's windowless OSR can't do DOM pointer lock), tracks the move keys, and
/// streams `fly-input` samples straight to the engine at the monitor refresh; no input sample
/// crosses CEF. The shell also owns every live end trigger (RMB release, Escape, focus loss) and
/// announces the end as a `fly-ended` event, so a stop can never be lost in transit. This hook
/// only starts the gesture from a viewport RMB press (passing the configured fly bindings),
/// mirrors the shell's gesture state into `flyingRef`, swallows the bound move keys so they don't
/// leak into DOM shortcuts, and stops the stream on unmount — the one end the shell can't see.
/// `flyingRef` is shared with the pointer-interaction effect, which stands down while the fly
/// owns the locked cursor.
export function useFlyCamera(
  hostRef: RefObject<HTMLDivElement | null>,
  flyingRef: RefObject<boolean>,
): void {
  useEffect(() => {
    const el = hostRef.current;
    if (!el) {
      return;
    }

    let flyEndedUnlisten: (() => void) | null = null;
    let disposed = false;

    const flyBindings = (): Record<string, string> => {
      const overrides = useEditorStore.getState().keyBindings;
      return {
        forward: bindingFor("camera.flyForward", overrides),
        back: bindingFor("camera.flyBack", overrides),
        left: bindingFor("camera.flyLeft", overrides),
        right: bindingFor("camera.flyRight", overrides),
        up: bindingFor("camera.flyUp", overrides),
        down: bindingFor("camera.flyDown", overrides),
      };
    };

    const onPointerDown = (event: PointerEvent): void => {
      if (event.button !== 2 || flyingRef.current) {
        return;
      }
      event.preventDefault();
      flyingRef.current = true;
      void invoke("fly_stream_start", flyBindings()).catch(() => {});
    };

    // The shell ends the gesture (RMB release / Escape / focus loss) and announces it here.
    void listen("fly-ended", () => {
      flyingRef.current = false;
    }).then((fn) => {
      if (disposed) {
        fn();
      } else {
        flyEndedUnlisten = fn;
      }
    });

    // The shell consumes the move keys; swallow them here so a held W/S/A/D doesn't also fire
    // DOM shortcuts while flying.
    const onKey = (event: KeyboardEvent): void => {
      if (!flyingRef.current) {
        return;
      }
      if (Object.values(flyBindings()).includes(event.code)) {
        event.preventDefault();
      }
    };

    const onContextMenu = (event: Event): void => event.preventDefault();

    el.addEventListener("pointerdown", onPointerDown);
    window.addEventListener("keydown", onKey);
    window.addEventListener("keyup", onKey);
    el.addEventListener("contextmenu", onContextMenu);

    return () => {
      disposed = true;
      flyEndedUnlisten?.();
      // Panel teardown is the one gesture end the shell can't observe on its own.
      if (flyingRef.current) {
        flyingRef.current = false;
        void invoke("fly_stream_stop").catch(() => {});
      }
      el.removeEventListener("pointerdown", onPointerDown);
      window.removeEventListener("keydown", onKey);
      window.removeEventListener("keyup", onKey);
      el.removeEventListener("contextmenu", onContextMenu);
    };
  }, [hostRef, flyingRef]);
}
