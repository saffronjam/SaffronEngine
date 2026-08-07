import { useEffect } from "react";
import { client } from "../../control/client";
import type { PlayState } from "../../state/store";
import { scriptKeyFromEvent, targetOwnsTextInput } from "./viewportInput";

/// Mirrors the held-key set to the running scripts while play is active; clears it on stop, blur, or
/// a hidden document so a held key can never strand.
export function useScriptInputForwarding(playState: PlayState): void {
  useEffect(() => {
    const pressed = new Set<string>();
    let lastSent = "";

    const send = (): void => {
      const keys = [...pressed].sort();
      const fingerprint = keys.join("\0");
      if (fingerprint === lastSent) {
        return;
      }
      lastSent = fingerprint;
      void client.scriptInput(keys).catch(() => {});
    };

    const clear = (): void => {
      if (pressed.size === 0 && lastSent === "") {
        return;
      }
      pressed.clear();
      send();
    };

    if (playState === "edit") {
      clear();
      return clear;
    }

    const onKeyDown = (event: KeyboardEvent): void => {
      if (targetOwnsTextInput(event.target)) {
        return;
      }
      const key = scriptKeyFromEvent(event);
      if (key === null) {
        return;
      }
      const size = pressed.size;
      pressed.add(key);
      if (pressed.size !== size) {
        send();
      }
    };

    const onKeyUp = (event: KeyboardEvent): void => {
      const key = scriptKeyFromEvent(event);
      if (key !== null && pressed.delete(key)) {
        send();
      }
    };

    const onVisibilityChange = (): void => {
      if (document.visibilityState !== "visible") {
        clear();
      }
    };

    window.addEventListener("keydown", onKeyDown);
    window.addEventListener("keyup", onKeyUp);
    window.addEventListener("blur", clear);
    document.addEventListener("visibilitychange", onVisibilityChange);
    return () => {
      clear();
      window.removeEventListener("keydown", onKeyDown);
      window.removeEventListener("keyup", onKeyUp);
      window.removeEventListener("blur", clear);
      document.removeEventListener("visibilitychange", onVisibilityChange);
    };
  }, [playState]);
}
