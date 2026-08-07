import { invoke } from "../../shell";
import { call } from "./call";
import type { AppDataInfo, EditorSettings, RecentProject, RecentProjects } from "./types";
import type {
  AppManifest,
  CommandParamsMap,
  CommandResultMap,
  ExportAppResult,
  ProjectInfo,
  ProjectStatus,
} from "../../protocol";

/// Project lifecycle, scene files, editor-local appdata, and the export pipeline.
export const projectCommands = {
  getProject(): Promise<ProjectInfo> {
    return call("get-project");
  },
  /// The live project-load phase + boot stage + progress. Allow-listed during `Loading`, so the
  /// loading-screen poll is always answered. See [`isBusyLoading`] for the discarded-command case.
  projectStatus(): Promise<ProjectStatus> {
    return call("project-status");
  },
  /// Abort the in-flight project load; the loader resets to `Unloaded` at its next step.
  cancelLoad(): Promise<ProjectStatus> {
    return call("cancel-load");
  },
  /// Kick off a non-blocking project create — returns the initial `Loading` status snapshot, not
  /// the finished project. Progress + completion come from [`projectStatus`] polling.
  newProject(name: string, displayName: string, root?: string): Promise<ProjectStatus> {
    const params: CommandParamsMap["new-project"] = {
      name,
      displayName,
    };
    if (root !== undefined && root !== "") {
      params.root = root;
    }
    return call("new-project", params);
  },
  /// Kick off a non-blocking project open — returns the initial `Loading` status snapshot; follow
  /// progress via [`projectStatus`].
  openProject(path: string): Promise<ProjectStatus> {
    return call("open-project", { path });
  },
  appDataInfo(): Promise<AppDataInfo> {
    return invoke<AppDataInfo>("app_data_info");
  },
  listRecentProjects(): Promise<RecentProjects> {
    return invoke<RecentProjects>("list_recent_projects");
  },
  rememberRecentProject(project: RecentProject): Promise<RecentProjects> {
    return invoke<RecentProjects>("remember_recent_project", { project });
  },
  /// Hide: drop the row from the recents MRU; the project's files are untouched.
  removeRecentProject(path: string): Promise<RecentProjects> {
    return invoke<RecentProjects>("remove_recent_project", { path });
  },
  /// Delete the project's directory tree and its recents row. The shell fences this to projects
  /// under the userdata root; anything else is refused.
  deleteProject(path: string): Promise<RecentProjects> {
    return invoke<RecentProjects>("delete_project", { path });
  },
  /// Whether `userdata/<name>` is free — the create form's collision probe.
  projectNameAvailable(name: string): Promise<boolean> {
    return invoke<boolean>("project_name_available", { name });
  },
  loadEditorSettings(): Promise<EditorSettings> {
    return invoke<EditorSettings>("load_editor_settings");
  },
  saveEditorSettings(settings: EditorSettings): Promise<void> {
    return invoke<void>("save_editor_settings", { settings });
  },

  /// Write catalog + scene to `path` (engine default `project.json` when omitted).
  saveProject(path?: string): Promise<ProjectInfo> {
    return call("save-project", path === undefined ? {} : { path });
  },
  /// The project's enabled asset-store connector ids (the `stores` block).
  getStores(): Promise<CommandResultMap["get-stores"]> {
    return call("get-stores");
  },
  /// Set the project's enabled asset-store connector ids (persisted by save-project).
  setStores(enabled: string[]): Promise<CommandResultMap["set-stores"]> {
    return call("set-stores", { enabled });
  },
  /// Cook the loaded project into a platform-native app at `outputDir`, writing `app` as the runtime
  /// manifest (`app.json`). Returns the staged path + any non-fatal cook warnings.
  exportApp(outputDir: string, app: AppManifest): Promise<ExportAppResult> {
    return call("export-app", { outputDir, app });
  },
  /// Kick off a non-blocking reload of the active project from its own path. Returns the initial
  /// `Loading` status snapshot; progress + completion come from [`projectStatus`] polling.
  reloadProject(): Promise<ProjectStatus> {
    return call("reload-project");
  },
  /// Write the scene only to `path` (required).
  saveScene(path: string): Promise<{ path: string }> {
    return call("save-scene", { path });
  },
  /// Load the scene only from `path` (required). Clears the engine's selection.
  loadScene(path: string): Promise<{ path: string }> {
    return call("load-scene", { path });
  },
  /// Capture a PNG. `viewport` is synchronous (`pending:false`); `window` is
};
