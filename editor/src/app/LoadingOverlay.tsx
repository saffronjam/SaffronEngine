/// Shown over the viewport region while the renderer is not ready — the phases where the native
/// surface is not yet mapped, or is gone after a crash. It MUST stay an absolutely-positioned
/// sibling layer rather than a Radix Dialog/portal, because it has to paint inline within the
/// viewport panel.
import { useState } from "react";
import { Loader2 } from "lucide-react";
import { client } from "../control/client";
import { useEditorStore } from "../state/store";
import { Button } from "@/components/ui/button";

export function LoadingOverlay() {
  const phase = useEditorStore((s) => s.engineStatus.phase);
  const error = useEditorStore((s) => s.engineStatus.error);
  const setPhase = useEditorStore((s) => s.setPhase);
  const [busy, setBusy] = useState(false);

  if (phase === "ready") {
    return null;
  }

  // The session boot intent for a recovery start: reopen the current project when one is
  // loaded, else let the host resolve the environment (an env/scratch boot that died).
  const recoveryIntent = () => {
    const path = useEditorStore.getState().project?.path;
    return path ? { path } : {};
  };

  // Retry: start a fresh session from idle/error. The phase flips to `attaching` only once the
  // child exists — before that the crash watchdog must stay quiet — and the ViewportPanel attach
  // probe takes it from there.
  const retry = async (): Promise<void> => {
    if (busy) {
      return;
    }
    setBusy(true);
    try {
      await client.sessionStart(recoveryIntent());
      setPhase("attaching");
    } catch (err) {
      setPhase("error", String(err));
    } finally {
      setBusy(false);
    }
  };

  // Restart: tear the current session down first, then start fresh.
  const restart = async (): Promise<void> => {
    if (busy) {
      return;
    }
    setBusy(true);
    try {
      await client.sessionStop().catch(() => {});
      await client.sessionStart(recoveryIntent());
      setPhase("attaching");
    } catch (err) {
      setPhase("error", String(err));
    } finally {
      setBusy(false);
    }
  };

  const message =
    phase === "starting"
      ? "Starting engine…"
      : phase === "attaching"
        ? "Attaching viewport…"
        : phase === "error"
          ? "Renderer unavailable"
          : "Preparing renderer…";

  return (
    <div
      className="absolute inset-0 z-10 flex items-center justify-center bg-background"
      role="status"
      aria-live="polite"
    >
      {phase === "error" ? (
        <div className="flex max-w-[480px] flex-col items-center gap-3.5 p-6 text-center">
          <div className="text-[15px] font-semibold text-destructive">{message}</div>
          {error ? (
            <pre className="max-h-40 w-full overflow-auto rounded-md border border-border bg-card px-3 py-2.5 text-left font-mono text-[11px] whitespace-pre-wrap text-muted-foreground">
              {error}
            </pre>
          ) : null}
          <div className="flex gap-2.5">
            <Button
              type="button"
              variant="outline"
              size="sm"
              onClick={() => void retry()}
              disabled={busy}
            >
              Retry
            </Button>
            <Button
              type="button"
              variant="outline"
              size="sm"
              onClick={() => void restart()}
              disabled={busy}
            >
              Restart
            </Button>
          </div>
        </div>
      ) : (
        <div className="flex flex-col items-center gap-3.5 text-muted-foreground">
          <Loader2 className="size-8 animate-spin text-primary" aria-hidden="true" />
          <div className="text-[13px]">{message}</div>
        </div>
      )}
    </div>
  );
}
