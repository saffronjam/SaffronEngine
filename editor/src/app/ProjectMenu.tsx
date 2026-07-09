/// Project-level file operations exposed from the topbar project selector.
import { useState } from "react";
import { getCurrentWindow, invoke, open, save } from "../shell";
import {
  ChevronDown,
  FilePlus2,
  FolderInput,
  FolderOpen,
  History,
  LogOut,
  Package,
  RefreshCw,
  Save,
  SaveAll,
} from "lucide-react";
import { client, type RecentProject } from "../control/client";
import { useEditorStore, withNativeDialog } from "../state/store";
import { errorText, notify } from "../lib/flash";
import { rememberProject } from "../lib/recentProjects";
import { Button } from "@/components/ui/button";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuSeparator,
  DropdownMenuSub,
  DropdownMenuSubContent,
  DropdownMenuSubTrigger,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";

const JSON_FILTER = [{ name: "Saffron Project", extensions: ["json"] }];

export function ProjectMenu() {
  const phase = useEditorStore((s) => s.engineStatus.phase);
  const startProjectLoad = useEditorStore((s) => s.startProjectLoad);
  const setProject = useEditorStore((s) => s.setProject);
  const project = useEditorStore((s) => s.project);
  const nativeDialogOpen = useEditorStore((s) => s.nativeDialogOpen);
  const devMode = useEditorStore((s) => s.devMode);
  const playState = useEditorStore((s) => s.playState);
  const setProjectModalOpen = useEditorStore((s) => s.setProjectModalOpen);
  const setExportModalOpen = useEditorStore((s) => s.setExportModalOpen);
  const [recents, setRecents] = useState<RecentProject[]>([]);

  const ready = phase === "ready";
  // Saving/loading/reloading are locked during play: open/reload swap the scene out
  // from under the play duplicate (the engine rejects them with "stop play first"), and
  // save is greyed too so play-mode tweaks are never mistaken for authored, saved state.
  const editing = playState === "edit";
  const label = project?.displayName ?? "No project";

  // "New Project" reuses the startup project picker (the one create/open flow); it is dismissable
  // because a project is already loaded.
  const newProject = (): void => {
    setProjectModalOpen(true);
  };

  // "Exit" closes the window; the shell's exit handler tears the engine down — the same path
  // as the titlebar close button.
  const exitApp = (): void => {
    void getCurrentWindow().close();
  };

  const saveProject = async (): Promise<void> => {
    try {
      const res = await client.saveProject();
      setProject(res);
      await rememberProject(res);
      notify(`Saved project: ${res.path}`);
    } catch (err) {
      notify(`Save project failed: ${errorText(err)}`);
    }
  };

  const saveProjectAs = async (): Promise<void> => {
    const path = await withNativeDialog(() =>
      save({ defaultPath: project?.path ?? "project.json", filters: JSON_FILTER }),
    );
    if (!path) {
      return;
    }
    try {
      const res = await client.saveProject(path);
      setProject(res);
      await rememberProject(res);
      notify(`Saved project: ${res.path}`);
    } catch (err) {
      notify(`Save project failed: ${errorText(err)}`);
    }
  };

  // The shared open path: kick off the non-blocking load. Progress + completion (setProject, scene
  // reset, recents) are owned by the startup modal's loading view + useProjectLoadPoll — the same
  // one seam as the picker. Used by "Open Project..." and every "Open Recent" entry.
  const loadProjectPath = (path: string): void => {
    void startProjectLoad({ kind: "open", path });
  };

  const openProject = async (): Promise<void> => {
    const selection = await withNativeDialog(() => open({ directory: true, multiple: false }));
    if (typeof selection !== "string") {
      return;
    }
    loadProjectPath(selection);
  };

  // Refresh the recent-project list when the menu opens (cheap; reflects saves/opens since last
  // time). The currently-open project is filtered out — reopening it is a no-op.
  const refreshRecents = (): void => {
    void client
      .listRecentProjects()
      .then((res) => {
        setRecents(res.projects.filter((p) => p.path !== project?.path));
      })
      .catch(() => {
        // Recents are a convenience; a failed read just leaves the submenu empty.
      });
  };

  // Opens the project root (project.json, src/ scripts, assets/) in VS Code via the
  // dedicated Rust command — `code` may live on the host, not in the toolbox.
  const openInVsCode = async (): Promise<void> => {
    if (!project) {
      return;
    }
    try {
      await invoke("open_in_vscode", { path: project.root });
    } catch (err) {
      notify(`Open in VS Code failed: ${errorText(err)}`);
    }
  };

  // Reveals the project root (project.json, src/ scripts, assets/) in the OS file manager
  // via the dedicated Rust command — the file manager lives on the host, not the toolbox.
  const openProjectFolder = async (): Promise<void> => {
    if (!project) {
      return;
    }
    try {
      await invoke("open_project_folder", { path: project.root });
    } catch (err) {
      notify(`Open project folder failed: ${errorText(err)}`);
    }
  };

  const reloadProject = (): void => {
    void startProjectLoad({ kind: "reload" });
  };

  return (
    <div className="flex min-w-0 items-center gap-2">
      <DropdownMenu
        onOpenChange={(isOpen) => {
          if (isOpen) {
            refreshRecents();
          }
        }}
      >
        <DropdownMenuTrigger asChild>
          <Button
            type="button"
            size="xs"
            variant="ghost"
            disabled={!ready || nativeDialogOpen}
            className="max-w-48 justify-start px-1.5 text-muted-foreground"
          >
            <span className="truncate">{label}</span>
            <ChevronDown />
          </Button>
        </DropdownMenuTrigger>
        <DropdownMenuContent align="start" className="min-w-60">
          <DropdownMenuItem onSelect={() => newProject()} disabled={!editing}>
            <FilePlus2 />
            <span>New Project...</span>
          </DropdownMenuItem>
          <DropdownMenuSeparator />
          <DropdownMenuItem onSelect={() => void saveProject()} disabled={!editing}>
            <Save />
            <span>Save Project</span>
          </DropdownMenuItem>
          <DropdownMenuItem onSelect={() => void saveProjectAs()} disabled={!editing}>
            <SaveAll />
            <span>Save Project As...</span>
          </DropdownMenuItem>
          <DropdownMenuItem onSelect={() => void openProject()} disabled={!editing}>
            <FolderInput />
            <span>Open Project...</span>
          </DropdownMenuItem>
          <DropdownMenuSub>
            <DropdownMenuSubTrigger disabled={!editing || recents.length === 0}>
              <History />
              <span>Open Recent</span>
            </DropdownMenuSubTrigger>
            <DropdownMenuSubContent className="max-w-80">
              {recents.length === 0 ? (
                <DropdownMenuItem disabled>No recent projects</DropdownMenuItem>
              ) : (
                recents.map((recent) => (
                  <DropdownMenuItem
                    key={recent.path}
                    onSelect={() => void loadProjectPath(recent.path)}
                  >
                    <div className="flex min-w-0 flex-col">
                      <span className="truncate">{recent.displayName}</span>
                      <span className="truncate text-xs text-muted-foreground">{recent.path}</span>
                    </div>
                  </DropdownMenuItem>
                ))
              )}
            </DropdownMenuSubContent>
          </DropdownMenuSub>
          <DropdownMenuItem onSelect={() => void openProjectFolder()} disabled={!project}>
            <FolderOpen />
            <span>Open Project Folder</span>
          </DropdownMenuItem>
          <DropdownMenuItem onSelect={() => void openInVsCode()} disabled={!project}>
            <VsCodeIcon className="size-4" />
            <span>Open in VS Code</span>
          </DropdownMenuItem>
          <DropdownMenuSeparator />
          <DropdownMenuItem
            onSelect={() => setExportModalOpen(true)}
            disabled={!editing || !project}
          >
            <Package />
            <span>Export App...</span>
          </DropdownMenuItem>
          {devMode ? (
            <>
              <DropdownMenuSeparator />
              <DropdownMenuItem
                onSelect={() => void reloadProject()}
                disabled={!project || !editing}
              >
                <RefreshCw />
                <span>Reload Project</span>
              </DropdownMenuItem>
            </>
          ) : null}
          <DropdownMenuSeparator />
          <DropdownMenuItem onSelect={() => exitApp()}>
            <LogOut />
            <span>Exit</span>
          </DropdownMenuItem>
        </DropdownMenuContent>
      </DropdownMenu>
    </div>
  );
}

function VsCodeIcon({ className }: { className?: string }) {
  return (
    <svg
      aria-hidden="true"
      className={className}
      viewBox="0 0 100 100"
      fill="none"
      xmlns="http://www.w3.org/2000/svg"
    >
      <mask id="vscode-menu-mask" maskUnits="userSpaceOnUse" x="0" y="0" width="100" height="100">
        <path
          fillRule="evenodd"
          clipRule="evenodd"
          d="M70.9119 99.3171C72.4869 99.9307 74.2828 99.8914 75.8725 99.1264L96.4608 89.2197C98.6242 88.1787 100 85.9892 100 83.5872V16.4133C100 14.0113 98.6243 11.8218 96.4609 10.7808L75.8725 0.873756C73.7862 -0.130129 71.3446 0.11576 69.5135 1.44695C69.252 1.63711 69.0028 1.84943 68.769 2.08341L29.3551 38.0415L12.1872 25.0096C10.589 23.7965 8.35363 23.8959 6.86933 25.2461L1.36303 30.2549C-0.452552 31.9064 -0.454633 34.7627 1.35853 36.417L16.2471 50.0001L1.35853 63.5832C-0.454633 65.2374 -0.452552 68.0938 1.36303 69.7453L6.86933 74.7541C8.35363 76.1043 10.589 76.2037 12.1872 74.9905L29.3551 61.9587L68.769 97.9167C69.3925 98.5406 70.1246 99.0104 70.9119 99.3171ZM75.0152 27.2989L45.1091 50.0001L75.0152 72.7012V27.2989Z"
          fill="white"
        />
      </mask>
      <g mask="url(#vscode-menu-mask)">
        <path
          d="M96.4614 10.7962L75.8569 0.875542C73.4719 -0.272773 70.6217 0.211611 68.75 2.08333L1.29858 63.5832C-0.515693 65.2373 -0.513607 68.0937 1.30308 69.7452L6.81272 74.754C8.29793 76.1042 10.5347 76.2036 12.1338 74.9905L93.3609 13.3699C96.086 11.3026 100 13.2462 100 16.6667V16.4275C100 14.0265 98.6246 11.8378 96.4614 10.7962Z"
          fill="#0065A9"
        />
        <path
          d="M96.4614 89.2038L75.8569 99.1245C73.4719 100.273 70.6217 99.7884 68.75 97.9167L1.29858 36.4169C-0.515693 34.7627 -0.513607 31.9063 1.30308 30.2548L6.81272 25.246C8.29793 23.8958 10.5347 23.7964 12.1338 25.0095L93.3609 86.6301C96.086 88.6974 100 86.7538 100 83.3334V83.5726C100 85.9735 98.6246 88.1622 96.4614 89.2038Z"
          fill="#007ACC"
        />
        <path
          d="M75.8578 99.1263C73.4721 100.274 70.6219 99.7885 68.75 97.9166C71.0564 100.223 75 98.5895 75 95.3278V4.67213C75 1.41039 71.0564 -0.223106 68.75 2.08329C70.6219 0.211402 73.4721 -0.273666 75.8578 0.873633L96.4587 10.7807C98.6234 11.8217 100 14.0112 100 16.4132V83.5871C100 85.9891 98.6234 88.1786 96.4586 89.2196L75.8578 99.1263Z"
          fill="#1F9CF0"
        />
        <path
          fillRule="evenodd"
          clipRule="evenodd"
          d="M70.8511 99.3171C72.4261 99.9306 74.2221 99.8913 75.8117 99.1264L96.4 89.2197C98.5634 88.1787 99.9392 85.9892 99.9392 83.5871V16.4133C99.9392 14.0112 98.5635 11.8217 96.4001 10.7807L75.8117 0.873695C73.7255 -0.13019 71.2838 0.115699 69.4527 1.44688C69.1912 1.63705 68.942 1.84937 68.7082 2.08335L29.2943 38.0414L12.1264 25.0096C10.5283 23.7964 8.29285 23.8959 6.80855 25.246L1.30225 30.2548C-0.513334 31.9064 -0.515415 34.7627 1.29775 36.4169L16.1863 50L1.29775 63.5832C-0.515415 65.2374 -0.513334 68.0937 1.30225 69.7452L6.80855 74.754C8.29285 76.1042 10.5283 76.2036 12.1264 74.9905L29.2943 61.9586L68.7082 97.9167C69.3317 98.5405 70.0638 99.0104 70.8511 99.3171ZM74.9544 27.2989L45.0483 50L74.9544 72.7012V27.2989Z"
          fill="url(#vscode-menu-gradient)"
          opacity="0.25"
          style={{ mixBlendMode: "overlay" }}
        />
      </g>
      <defs>
        <linearGradient
          id="vscode-menu-gradient"
          x1="49.9392"
          y1="0.257812"
          x2="49.9392"
          y2="99.7423"
          gradientUnits="userSpaceOnUse"
        >
          <stop stopColor="white" />
          <stop offset="1" stopColor="white" stopOpacity="0" />
        </linearGradient>
      </defs>
    </svg>
  );
}
