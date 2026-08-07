/// The launcher's picker: a full-width recents MRU with per-row Hide/Delete, and a card-swap
/// create form whose on-disk name is derived from the display name — never typed. Handlers kick
/// off the shared non-blocking load thunk (which starts a host session when none is live); this
/// component holds only list/form state, never the load itself.
import { useEffect, useRef, useState } from "react";
import { open } from "../shell";
import { EllipsisVertical, FolderOpen, Plus } from "lucide-react";
import { client, type AppDataInfo, type RecentProject } from "../control/client";
import { useEditorStore, withNativeDialog } from "../state/store";
import { errorText, notifyError } from "../lib/flash";
import { deriveProjectSlug, validProjectName } from "./projectName";
import { Button } from "@/components/ui/button";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";

const PROJECT_JSON_FILTER = [
  { name: "Saffron Project", extensions: ["json"] },
  { name: "All Files", extensions: ["*"] },
];

/// How long the create form waits after a keystroke before probing the slug for a collision.
const COLLISION_PROBE_MS = 250;

function startOpen(path: string): void {
  void useEditorStore.getState().startProjectLoad({ kind: "open", path });
}

export function PickerCard() {
  const [view, setView] = useState<"list" | "create">("list");
  const [info, setInfo] = useState<AppDataInfo | null>(null);
  const [recents, setRecents] = useState<RecentProject[]>([]);
  const [confirmDelete, setConfirmDelete] = useState<RecentProject | null>(null);

  const refreshRecents = async (): Promise<void> => {
    try {
      const [nextInfo, nextRecents] = await Promise.all([
        client.appDataInfo(),
        client.listRecentProjects(),
      ]);
      setInfo(nextInfo);
      setRecents(nextRecents.projects);
    } catch (err) {
      notifyError(errorText(err));
    }
  };

  useEffect(() => {
    void refreshRecents();
  }, []);

  const openProjectFile = async (): Promise<void> => {
    const selection = await withNativeDialog(() =>
      open({ multiple: false, filters: PROJECT_JSON_FILTER }),
    );
    if (typeof selection === "string") {
      startOpen(selection);
    }
  };

  const hide = (project: RecentProject): void => {
    void client
      .removeRecentProject(project.path)
      .then((next) => setRecents(next.projects))
      .catch((err: unknown) => notifyError(errorText(err)));
  };

  const deleteConfirmed = (project: RecentProject): void => {
    setConfirmDelete(null);
    void client
      .deleteProject(project.path)
      .then((next) => setRecents(next.projects))
      .catch((err: unknown) => notifyError(errorText(err)));
  };

  /// Delete is fenced to the userdata root; anything opened from elsewhere only offers Hide.
  const deletable = (project: RecentProject): boolean =>
    info !== null && project.path.startsWith(`${info.userdataDir}/`);

  if (view === "create") {
    return <CreateForm onBack={() => setView("list")} userdataDir={info?.userdataDir ?? ""} />;
  }
  return (
    <div className="flex h-[440px] flex-col">
      <header className="mb-4 flex flex-none items-center justify-between gap-3">
        <div className="space-y-1">
          <h1 className="text-lg font-semibold">Open a project</h1>
          <p className="text-sm text-muted-foreground">
            Choose a recent project or create a new one.
          </p>
        </div>
        <div className="flex flex-none gap-2">
          <Button type="button" variant="outline" onClick={() => void openProjectFile()}>
            <FolderOpen />
            Open
          </Button>
          <Button type="button" onClick={() => setView("create")}>
            <Plus />
            New
          </Button>
        </div>
      </header>

      <section className="relative flex min-h-0 min-w-0 flex-1 flex-col rounded-md border border-border bg-background/60">
        <div className="flex h-9 flex-none items-center border-b border-border px-3">
          <span className="text-xs font-medium uppercase text-muted-foreground">Recent</span>
        </div>
        <div className="min-h-0 flex-1 overflow-auto p-2">
          {recents.length === 0 ? (
            <div className="flex h-40 items-center justify-center text-sm text-muted-foreground">
              No recent projects
            </div>
          ) : (
            <div className="space-y-1">
              {recents.map((project) => (
                <div
                  key={project.path}
                  className="group flex w-full min-w-0 items-center gap-1 rounded-md hover:bg-accent"
                >
                  <button
                    type="button"
                    className="flex min-w-0 flex-1 flex-col gap-1 px-3 py-2 text-left"
                    onClick={() => startOpen(project.path)}
                  >
                    <span className="block max-w-full truncate text-sm font-medium">
                      {project.displayName}
                    </span>
                    <span className="block max-w-full truncate font-mono text-[11px] text-muted-foreground">
                      {project.path}
                    </span>
                  </button>
                  <DropdownMenu>
                    <DropdownMenuTrigger asChild>
                      <Button
                        type="button"
                        size="icon-xs"
                        variant="ghost"
                        className="mr-2 flex-none opacity-0 group-hover:opacity-100 data-[state=open]:opacity-100"
                        aria-label={`Actions for ${project.displayName}`}
                      >
                        <EllipsisVertical />
                      </Button>
                    </DropdownMenuTrigger>
                    <DropdownMenuContent align="end">
                      <DropdownMenuItem onSelect={() => hide(project)}>Hide</DropdownMenuItem>
                      {deletable(project) ? (
                        <DropdownMenuItem
                          variant="destructive"
                          onSelect={() => setConfirmDelete(project)}
                        >
                          Delete…
                        </DropdownMenuItem>
                      ) : null}
                    </DropdownMenuContent>
                  </DropdownMenu>
                </div>
              ))}
            </div>
          )}
        </div>

        {confirmDelete ? (
          <DeleteConfirm
            project={confirmDelete}
            onCancel={() => setConfirmDelete(null)}
            onConfirm={() => deleteConfirmed(confirmDelete)}
          />
        ) : null}
      </section>

      <footer className="mt-4 flex min-w-0 flex-none items-center justify-between gap-3">
        <span className="min-w-0 flex-1 truncate font-mono text-[11px] text-muted-foreground">
          {info ? info.userdataDir : ""}
        </span>
      </footer>
    </div>
  );
}

/// The destructive confirmation, scoped inside the recents section (no whole-document modal
/// machinery): names the folder that will be removed from disk.
function DeleteConfirm({
  project,
  onCancel,
  onConfirm,
}: {
  project: RecentProject;
  onCancel(): void;
  onConfirm(): void;
}) {
  const folder = project.path.replace(/\/project\.json$/, "");
  return (
    <div className="absolute inset-0 z-10 flex items-center justify-center rounded-md bg-background/50 p-4">
      <div className="w-full max-w-md space-y-3 rounded-md border border-border bg-card p-4 shadow-lg">
        <div className="space-y-1">
          <h2 className="text-sm font-semibold">Delete {project.displayName}?</h2>
          <p className="text-xs text-muted-foreground">
            This removes the project and everything inside it from disk. This cannot be undone.
          </p>
        </div>
        <pre className="overflow-x-auto rounded-md bg-background/60 p-2 font-mono text-[11px] text-muted-foreground">
          {folder}
        </pre>
        <div className="flex justify-end gap-2">
          <Button type="button" size="sm" variant="outline" onClick={onCancel}>
            Cancel
          </Button>
          <Button type="button" size="sm" variant="destructive" onClick={onConfirm}>
            Delete
          </Button>
        </div>
      </div>
    </div>
  );
}

/// The create form: one display-name input; the on-disk name is derived, previewed live, and
/// probed for a collision against the userdata root.
function CreateForm({ onBack, userdataDir }: { onBack(): void; userdataDir: string }) {
  const [name, setName] = useState("");
  const [taken, setTaken] = useState(false);
  const probeSeq = useRef(0);

  const slug = deriveProjectSlug(name);
  const empty = name.trim().length === 0;
  const usable = slug.length > 0 && validProjectName(slug);

  // Debounced collision probe; a stale reply never overwrites a newer one, and the error clears
  // the moment the slug changes — it only ever describes the slug currently in the field.
  useEffect(() => {
    setTaken(false);
    if (!usable) {
      return;
    }
    const seq = ++probeSeq.current;
    const timer = setTimeout(() => {
      void client
        .projectNameAvailable(slug)
        .then((available) => {
          if (probeSeq.current === seq) {
            setTaken(!available);
          }
        })
        .catch(() => {
          // Probe failure is not a blocker; creation still validates engine-side at boot.
          if (probeSeq.current === seq) {
            setTaken(false);
          }
        });
    }, COLLISION_PROBE_MS);
    return () => clearTimeout(timer);
  }, [slug, usable]);

  const create = (): void => {
    if (!usable || taken) {
      return;
    }
    void useEditorStore
      .getState()
      .startProjectLoad({ kind: "new", name: slug, displayName: name.trim() });
  };

  return (
    <div className="flex h-[440px] flex-col">
      <header className="mb-4 flex h-[52px] flex-none items-center">
        <h1 className="text-lg font-semibold">Create a project</h1>
      </header>

      <div className="flex min-h-0 flex-1 flex-col gap-4">
        <div className="space-y-2">
          <Label htmlFor="project-display-name">Name</Label>
          <Input
            id="project-display-name"
            value={name}
            autoFocus
            onChange={(event) => setName(event.target.value)}
            onKeyDown={(event) => {
              if (event.key === "Enter") {
                create();
              }
            }}
            placeholder="My Project"
            aria-invalid={(!empty && !usable) || taken}
          />
          {empty ? null : usable ? (
            <p className="font-mono text-[11px] text-muted-foreground">
              Will be created at {userdataDir}/{slug}
            </p>
          ) : (
            <p className="text-xs text-destructive">The name needs at least one letter or digit.</p>
          )}
          {taken ? (
            <p className="text-xs text-destructive">A project named “{slug}” already exists.</p>
          ) : null}
        </div>
      </div>

      <footer className="mt-4 flex flex-none justify-end gap-2">
        <Button type="button" variant="outline" onClick={onBack}>
          Back
        </Button>
        <Button type="button" disabled={!usable || taken} onClick={create}>
          <Plus />
          Create Project
        </Button>
      </footer>
    </div>
  );
}
