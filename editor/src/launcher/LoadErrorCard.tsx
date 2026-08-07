/// The launcher's load-failure view: the engine's failure message + Retry (replay the stashed
/// request) / Back to the picker.
import { useEditorStore } from "../state/store";
import { Button } from "@/components/ui/button";

export function LoadErrorCard() {
  const error = useEditorStore((s) => s.projectLoad.error);
  const request = useEditorStore((s) => s.projectLoad.request);
  const startProjectLoad = useEditorStore((s) => s.startProjectLoad);
  const setProjectLoad = useEditorStore((s) => s.setProjectLoad);
  const setLauncherOpen = useEditorStore((s) => s.setLauncherOpen);

  const back = (): void => {
    setProjectLoad({ phase: "idle" });
    // Hold the launcher open over a still-loaded project (a failed menu switch), so the user
    // lands on the picker either way.
    setLauncherOpen(true);
  };

  return (
    <>
      <header className="mb-2 space-y-1">
        <h1 className="text-lg font-semibold text-destructive">Project load failed</h1>
        <p className="text-sm text-muted-foreground">The project could not be loaded.</p>
      </header>

      <div className="py-2">
        <pre className="max-h-48 overflow-auto rounded-md bg-background/60 p-3 font-mono text-[11px] text-destructive whitespace-pre-wrap">
          {error || "Unknown error."}
        </pre>
      </div>

      <footer className="flex justify-end gap-2">
        <Button type="button" size="sm" variant="outline" onClick={back}>
          Back to picker
        </Button>
        <Button
          type="button"
          size="sm"
          variant="outline"
          disabled={!request}
          onClick={() => {
            if (request) {
              void startProjectLoad(request);
            }
          }}
        >
          Retry
        </Button>
      </footer>
    </>
  );
}
