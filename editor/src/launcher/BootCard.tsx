/// The launcher's boot view: a determinate progress bar (or a spinner while the stage is
/// indeterminate) + the engine-supplied status line + Cancel. Subscribes the progress primitives
/// here so a ~10 Hz progress tick never re-renders the picker.
import { useState } from "react";
import { Loader2 } from "lucide-react";
import { client } from "../control/client";
import { useEditorStore } from "../state/store";
import { errorText, notifyError } from "../lib/flash";
import { cn } from "@/lib/utils";
import { Button } from "@/components/ui/button";
import { Progress } from "@/components/ui/progress";

export function BootCard() {
  const label = useEditorStore((s) => s.projectLoad.label);
  const done = useEditorStore((s) => s.projectLoad.done);
  const total = useEditorStore((s) => s.projectLoad.total);
  const currentItem = useEditorStore((s) => s.projectLoad.currentItem);
  const finalizing = useEditorStore((s) => s.projectLoad.finalizing ?? false);
  const [cancelling, setCancelling] = useState(false);

  const determinate = total > 0;
  const percent = determinate ? Math.round((done / total) * 100) : 0;

  const cancel = (): void => {
    setCancelling(true);
    const store = useEditorStore.getState();
    if (store.project === null) {
      // A session boot has no editor to return to: cancelling ends the session, and the
      // launcher settles back to the picker.
      void client
        .sessionStop()
        .then(() => {
          const s = useEditorStore.getState();
          s.setProjectLoadCancelling(false);
          s.setProjectLoad({ phase: "idle" });
          s.setPhase("idle");
        })
        .catch((err: unknown) => {
          setCancelling(false);
          notifyError(errorText(err));
        });
      return;
    }
    // An in-session load (menu open/reload): mark the intent so the poll refuses to complete
    // even if the load already reached `ready`, then ask the engine to abort. The poll settles
    // back; a rejected kick leaves it running, so re-enable + toast.
    store.setProjectLoadCancelling(true);
    void client.cancelLoad().catch((err: unknown) => {
      setCancelling(false);
      useEditorStore.getState().setProjectLoadCancelling(false);
      notifyError(errorText(err));
    });
  };

  return (
    <>
      <header className="mb-2">
        <h1 className="text-lg font-semibold">Loading project</h1>
      </header>

      <div className="flex min-h-[180px] flex-col items-center justify-center gap-4 px-6 py-4">
        {determinate ? (
          <Progress value={percent} className="w-full max-w-sm" />
        ) : (
          <Loader2 className="size-6 animate-spin text-muted-foreground" />
        )}
        {/* On completion the bar sits at 100% while this status text fades out, then the launcher
            dismisses — no jarring text swap to "Ready". */}
        <div
          className={cn(
            "flex w-full max-w-sm flex-col items-center gap-1 text-center transition-opacity duration-300",
            finalizing ? "opacity-0" : "opacity-100",
          )}
        >
          <span className="text-sm text-foreground">
            {label || "Preparing…"}
            {determinate ? <span className="ml-2 text-muted-foreground">{percent}%</span> : null}
          </span>
          {currentItem ? (
            <span className="max-w-full truncate font-mono text-[11px] text-muted-foreground">
              {currentItem}
            </span>
          ) : null}
        </div>
      </div>

      <footer
        className={cn(
          "flex justify-center transition-opacity duration-300",
          finalizing ? "opacity-0" : "opacity-100",
        )}
      >
        <Button
          type="button"
          size="sm"
          variant="outline"
          disabled={cancelling || finalizing}
          onClick={cancel}
        >
          {cancelling ? "Cancelling…" : "Cancel"}
        </Button>
      </footer>
    </>
  );
}
