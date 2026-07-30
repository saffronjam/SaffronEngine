import { useEffect } from "react";
import { client } from "../../control/client";
import { useEditorStore } from "../../state/store";

/// Drives the viewport-ready handshake.
export function useEngineAttachProbe(): void {
  const phase = useEditorStore((s) => s.engineStatus.phase);
  const setPhase = useEditorStore((s) => s.setPhase);

  // Readiness is owned here: this probe is the SINGLE source of truth for the viewport being live.
  // It polls the control plane and flips the phase to `ready` once the engine can attach. It runs
  // whenever the viewport is NOT ready — initial boot, or a legitimate re-attach that reset the
  // phase away from `ready` — so recovery is automatic: nothing has to guard against or replay the
  // backend's phase events (the backend no longer drives the startup attach at all; it only reports
  // failures). The `attaching` label is set here for the `idle` boot state.
  //
  // Each attempt is bounded by a timeout so a single slow/dropped `invoke` (e.g. the shell's main
  // thread busy under the first render burst) can't wedge the attach — the next attempt fires a
  // fresh call. `cancelled` makes any pending retry a no-op after the phase changes or unmount.
  useEffect(() => {
    if (phase === "ready" || phase === "error") {
      return;
    }
    if (phase === "idle") {
      setPhase("attaching");
      return;
    }

    let cancelled = false;
    const PROBE_TIMEOUT_MS = 1500;

    const probe = async (): Promise<void> => {
      if (cancelled) {
        return;
      }
      try {
        await Promise.race([
          client.viewportNativeInfo(),
          new Promise((_resolve, reject) => {
            setTimeout(() => reject(new Error("viewport probe timed out")), PROBE_TIMEOUT_MS);
          }),
        ]);
      } catch {
        if (!cancelled) {
          setTimeout(() => void probe(), 150);
        }
        return;
      }
      if (!cancelled) {
        setPhase("ready");
      }
    };

    void probe();

    return () => {
      cancelled = true;
    };
  }, [phase, setPhase]);
}
