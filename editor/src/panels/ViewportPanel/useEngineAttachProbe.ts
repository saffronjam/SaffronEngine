import { useEffect } from "react";
import { client } from "../../control/client";
import { useEditorStore } from "../../state/store";

/// Drives the viewport-ready handshake.
export function useEngineAttachProbe(): void {
  const phase = useEditorStore((s) => s.engineStatus.phase);
  const setPhase = useEditorStore((s) => s.setPhase);

  // Readiness is owned here: this probe is the SINGLE source of truth for the viewport being live.
  // It polls the control plane and flips the phase to `ready` once the engine can attach. It runs
  // whenever a session is coming up — the session start flips the phase to `attaching` — or after
  // a legitimate re-attach reset the phase away from `ready`, so recovery is automatic: nothing
  // has to guard against or replay the backend's phase events (the backend only reports failures).
  // `idle` means no session exists (nothing to probe, and the crash watchdog must stay quiet).
  //
  // Each attempt is bounded by a timeout so a single slow/dropped `invoke` (e.g. the shell's main
  // thread busy under the first render burst) can't wedge the attach — the next attempt fires a
  // fresh call. `cancelled` makes any pending retry a no-op after the phase changes or unmount.
  useEffect(() => {
    if (phase === "ready" || phase === "error" || phase === "idle") {
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
