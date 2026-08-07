/// The launcher's crash view: an unrequested host exit, with the exit code, the tail of the host
/// log, and Restart / Back actions. The session process is already gone when this shows; Back
/// still runs `session_stop` for its socket/shm cleanup.
import { client } from "../control/client";
import { useEditorStore } from "../state/store";
import { Button } from "@/components/ui/button";

export function CrashCard() {
  const crash = useEditorStore((s) => s.sessionCrash);
  const setSessionCrash = useEditorStore((s) => s.setSessionCrash);

  if (!crash) {
    return null;
  }

  const back = (): void => {
    void client.sessionStop().catch(() => {});
    setSessionCrash(null);
  };

  const restart = (): void => {
    const path = crash.projectPath;
    void client.sessionStop().catch(() => {});
    setSessionCrash(null);
    if (path) {
      void useEditorStore.getState().startProjectLoad({ kind: "open", path });
    }
  };

  return (
    <>
      <header className="mb-2 space-y-1">
        <h1 className="text-lg font-semibold text-destructive">Engine crashed</h1>
        <p className="text-sm text-muted-foreground">
          The project session ended unexpectedly (exit code {crash.code}).
        </p>
      </header>

      {crash.logTail.length > 0 ? (
        <div className="py-2">
          <pre className="max-h-56 overflow-auto rounded-md bg-background/60 p-3 font-mono text-[11px] text-muted-foreground whitespace-pre-wrap">
            {crash.logTail.join("\n")}
          </pre>
        </div>
      ) : null}

      <footer className="flex justify-end gap-2">
        <Button type="button" size="sm" variant="outline" onClick={back}>
          Back to projects
        </Button>
        <Button
          type="button"
          size="sm"
          variant="outline"
          disabled={!crash.projectPath}
          onClick={restart}
        >
          Restart project
        </Button>
      </footer>
    </>
  );
}
