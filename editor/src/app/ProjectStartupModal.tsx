import { useEffect, useMemo, useRef, useState } from "react";
import { open } from "@tauri-apps/plugin-dialog";
import { FolderOpen, Loader2, Plus, RefreshCcw } from "lucide-react";
import { client, type AppDataInfo, type RecentProject } from "../control/client";
import { useEditorStore, withNativeDialog } from "../state/store";
import { errorText, notifyError } from "../lib/flash";
import { cn } from "@/lib/utils";
import { Button } from "@/components/ui/button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Progress } from "@/components/ui/progress";
import { Tooltip, TooltipContent, TooltipTrigger } from "@/components/ui/tooltip";

const PROJECT_JSON_FILTER = [{ name: "Saffron Project", extensions: ["json"] }];

/// The Dialog's exit animation length (`duration-200` on `DialogContent`). The viewport reveal
/// waits this out so the loaded scene doesn't pop in behind the still-fading modal.
const MODAL_EXIT_MS = 220;

interface ProjectStartupModalProps {
  open: boolean;
}

/// The startup surface: a project picker that also hosts the non-blocking load view. The picker
/// kicks off `startProjectLoad` (which returns immediately); progress + completion are driven by the
/// `useProjectLoadPoll` hook mounted in `App.tsx`, so this component only reflects `projectLoad`.
export function ProjectStartupModal({ open: modalOpen }: ProjectStartupModalProps) {
  const setViewportHidden = useEditorStore((s) => s.setViewportHidden);
  const setProjectModalOpen = useEditorStore((s) => s.setProjectModalOpen);
  // At startup there is no project yet and a choice is mandatory, so the picker is locked open.
  // Reopened from the project menu (a project already loaded) it is dismissable instead.
  const dismissable = useEditorStore((s) => s.project !== null);
  const loadPhase = useEditorStore((s) => s.projectLoad.phase);

  const loading = loadPhase === "loading";
  const errored = loadPhase === "error";
  // The Dialog is open when the picker is up OR a load is in flight — so a bootstrap/menu load,
  // which never opens the picker, still shows the loading view.
  const dialogOpen = modalOpen || loading || errored;

  // The reparented X11 viewport always paints over the webview; park it off-screen while the dialog
  // is open (across picker → loading → close) so the dialog is actually visible over the viewport.
  // On open, park immediately. On close, hold the park until the modal's exit animation finishes,
  // so the loaded scene fades in *after* the dialog is gone instead of popping in behind it.
  useEffect(() => {
    if (dialogOpen) {
      setViewportHidden(true);
      return;
    }
    const timer = setTimeout(() => setViewportHidden(false), MODAL_EXIT_MS);
    return () => clearTimeout(timer);
  }, [dialogOpen, setViewportHidden]);

  // Non-dismissable while a load runs or after it failed — the only exits are Cancel / Retry / Back.
  // Back in the picker, restore the normal dismissable-close behavior.
  const dismissableNow = dialogOpen && !loading && !errored && dismissable;

  // Which body to show. Freeze it while the dialog is closing: Radix keeps the content mounted for
  // its exit animation, and on completion `loading` flips false a frame before the dialog is gone —
  // without this the picker would flash for that frame behind the fading modal.
  const view: "error" | "loading" | "picker" = errored ? "error" : loading ? "loading" : "picker";
  const stickyView = useRef(view);
  if (dialogOpen) {
    stickyView.current = view;
  }
  const shownView = stickyView.current;

  return (
    <Dialog
      open={dialogOpen}
      onOpenChange={(next) => {
        if (!next && dismissableNow) {
          setProjectModalOpen(false);
        }
      }}
    >
      <DialogContent showCloseButton={dismissableNow} className="sm:max-w-[760px]">
        {shownView === "error" ? (
          <ProjectErrorView modalOpen={modalOpen} />
        ) : shownView === "loading" ? (
          <ProjectLoadingView />
        ) : (
          <ProjectPickerView onOpenPath={startOpen} onCreate={startNew} />
        )}
      </DialogContent>
    </Dialog>
  );

  function startOpen(path: string): void {
    void useEditorStore.getState().startProjectLoad({ kind: "open", path });
  }
  function startNew(name: string, display: string): void {
    void useEditorStore.getState().startProjectLoad({ kind: "new", name, displayName: display });
  }
}

/// The picker: recent list + create/open sections. Its handlers kick off the shared non-blocking
/// load thunk; it holds only pre-flight validation state (a name check), never the load itself.
function ProjectPickerView({
  onOpenPath,
  onCreate,
}: {
  onOpenPath(path: string): void;
  onCreate(name: string, displayName: string): void;
}) {
  const [info, setInfo] = useState<AppDataInfo | null>(null);
  const [recents, setRecents] = useState<RecentProject[]>([]);
  const [name, setName] = useState("");
  const [displayName, setDisplayName] = useState("");
  const [status, setStatus] = useState<string | null>(null);

  const nameError = useMemo(() => {
    if (name.length === 0) {
      return null;
    }
    return validProjectName(name)
      ? null
      : "Use lowercase letters, digits, and hyphens; start and end with a letter or digit.";
  }, [name]);

  const refreshRecents = async (): Promise<void> => {
    try {
      const [nextInfo, nextRecents] = await Promise.all([
        client.appDataInfo(),
        client.listRecentProjects(),
      ]);
      setInfo(nextInfo);
      setRecents(nextRecents.projects);
    } catch (err) {
      setStatus(errorText(err));
    }
  };

  useEffect(() => {
    void refreshRecents();
  }, []);

  const createProject = (): void => {
    if (!validProjectName(name)) {
      setStatus("Enter a valid project name.");
      return;
    }
    setStatus(null);
    onCreate(name, displayName.trim());
  };

  const openProjectDirectory = async (): Promise<void> => {
    const selection = await withNativeDialog(() => open({ directory: true, multiple: false }));
    if (typeof selection === "string") {
      onOpenPath(selection);
    }
  };

  const openProjectFile = async (): Promise<void> => {
    const selection = await withNativeDialog(() =>
      open({ multiple: false, filters: PROJECT_JSON_FILTER }),
    );
    if (typeof selection === "string") {
      onOpenPath(selection);
    }
  };

  return (
    <>
      <DialogHeader>
        <DialogTitle>Open a project</DialogTitle>
        <DialogDescription>Choose a recent project or create a new one.</DialogDescription>
      </DialogHeader>

      <div className="grid min-h-[320px] min-w-0 gap-4 md:grid-cols-[minmax(0,1fr)_minmax(160px,10rem)]">
        <section className="min-h-0 min-w-0 rounded-md border border-border bg-card">
          <div className="flex h-9 items-center justify-between border-b border-border px-3">
            <span className="text-xs font-medium uppercase text-muted-foreground">Recent</span>
            <Tooltip>
              <TooltipTrigger asChild>
                <Button
                  type="button"
                  size="icon-xs"
                  variant="ghost"
                  onClick={() => void refreshRecents()}
                >
                  <RefreshCcw />
                </Button>
              </TooltipTrigger>
              <TooltipContent>Refresh recent projects</TooltipContent>
            </Tooltip>
          </div>
          <div className="max-h-[280px] overflow-auto p-2">
            {recents.length === 0 ? (
              <div className="flex h-28 items-center justify-center text-sm text-muted-foreground">
                No recent projects
              </div>
            ) : (
              <div className="space-y-1">
                {recents.map((project) => (
                  <button
                    key={project.path}
                    type="button"
                    className="flex w-full min-w-0 flex-col gap-1 rounded-md px-3 py-2 text-left hover:bg-accent disabled:opacity-50"
                    onClick={() => onOpenPath(project.path)}
                  >
                    <span className="block max-w-full truncate text-sm font-medium">
                      {project.displayName}
                    </span>
                    <span className="block max-w-full truncate font-mono text-[11px] text-muted-foreground">
                      {project.path}
                    </span>
                  </button>
                ))}
              </div>
            )}
          </div>
        </section>

        <section className="min-w-0 space-y-4">
          <div className="space-y-2">
            <Label htmlFor="project-name">Project name</Label>
            <Input
              id="project-name"
              value={name}
              onChange={(event) => setName(event.target.value)}
              placeholder="a-name-like-this"
              aria-invalid={nameError !== null}
            />
            {nameError ? <p className="text-xs text-destructive">{nameError}</p> : null}
          </div>
          <div className="space-y-2">
            <Label htmlFor="project-display-name">Display name</Label>
            <Input
              id="project-display-name"
              value={displayName}
              onChange={(event) => setDisplayName(event.target.value)}
              placeholder="A Name Like This"
            />
          </div>
          <Button
            type="button"
            className="w-full"
            onClick={createProject}
            disabled={!validProjectName(name)}
          >
            <Plus />
            Create Project
          </Button>

          <div className="grid gap-2 pt-2">
            <Button type="button" variant="outline" onClick={() => void openProjectDirectory()}>
              <FolderOpen />
              Open Folder
            </Button>
            <Button type="button" variant="outline" onClick={() => void openProjectFile()}>
              <FolderOpen />
              Open project.json
            </Button>
          </div>
        </section>
      </div>

      <DialogFooter className="min-w-0 items-center justify-between gap-3 sm:justify-between">
        <span className="min-w-0 flex-1 truncate font-mono text-[11px] text-muted-foreground">
          {info ? info.userdataDir : ""}
        </span>
        {status ? (
          <span className="max-w-[320px] truncate text-xs text-destructive">{status}</span>
        ) : null}
      </DialogFooter>
    </>
  );
}

/// The loading view: a determinate progress bar (or a spinner while the stage is indeterminate) +
/// the engine-supplied status line + Cancel. Subscribes the progress primitives here so a ~10 Hz
/// progress tick never re-renders the picker.
function ProjectLoadingView() {
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
    // Mark the intent so the poll refuses to complete even if the load already reached `ready`
    // (a fast load can finish before this click lands), then ask the engine to abort. The poll
    // settles the modal back to the picker; a rejected kick leaves it running, so re-enable + toast.
    useEditorStore.getState().setProjectLoadCancelling(true);
    void client.cancelLoad().catch((err) => {
      setCancelling(false);
      useEditorStore.getState().setProjectLoadCancelling(false);
      notifyError(errorText(err));
    });
  };

  return (
    <>
      <DialogHeader>
        <DialogTitle>Loading project</DialogTitle>
        <DialogDescription>
          Bringing the project up — this can take a moment for large scenes.
        </DialogDescription>
      </DialogHeader>

      <div className="flex min-h-[180px] flex-col items-center justify-center gap-4 px-6 py-4">
        {determinate ? (
          <Progress value={percent} className="w-full max-w-sm" />
        ) : (
          <Loader2 className="size-6 animate-spin text-muted-foreground" />
        )}
        {/* On completion the bar sits at 100% while this status text fades out, then the modal
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

      <DialogFooter
        className={cn(
          "transition-opacity duration-300 sm:justify-center",
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
      </DialogFooter>
    </>
  );
}

/// The error view: the engine's failure message + Retry (replay the stashed request) / Back.
function ProjectErrorView({ modalOpen }: { modalOpen: boolean }) {
  const error = useEditorStore((s) => s.projectLoad.error);
  const request = useEditorStore((s) => s.projectLoad.request);
  const startProjectLoad = useEditorStore((s) => s.startProjectLoad);
  const setProjectLoad = useEditorStore((s) => s.setProjectLoad);
  const setProjectModalOpen = useEditorStore((s) => s.setProjectModalOpen);

  const back = (): void => {
    setProjectLoad({ phase: "idle" });
    // A bootstrap/menu load has no picker behind it — reveal one so the user can pick another.
    if (!modalOpen) {
      setProjectModalOpen(true);
    }
  };

  return (
    <>
      <DialogHeader>
        <DialogTitle className="text-destructive">Project load failed</DialogTitle>
        <DialogDescription>The project could not be loaded.</DialogDescription>
      </DialogHeader>

      <div className="px-2 py-2">
        <pre className="max-h-48 overflow-auto rounded-md bg-card p-3 font-mono text-[11px] text-destructive whitespace-pre-wrap">
          {error || "Unknown error."}
        </pre>
      </div>

      <DialogFooter className="gap-2 sm:justify-end">
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
      </DialogFooter>
    </>
  );
}

export function validProjectName(name: string): boolean {
  if (name.length < 1 || name.length > 63) {
    return false;
  }
  return /^[a-z0-9](?:[a-z0-9-]*[a-z0-9])?$/.test(name);
}
