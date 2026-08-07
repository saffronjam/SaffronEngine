import type { CommandParamsMap, ProfilerModeResult } from "../../protocol";

/// The GPU profiler depth (off keeps the present-only host at baseline cost).
export type ProfilerMode = ProfilerModeResult["mode"];

/// Which viewport a per-view command targets. Each view has its own render target, shm ring, and
/// subsurface; the wire tokens must match the engine's `viewIdFromWire`.
export type ViewId = "scene" | "assetPreview";

/// The viewport panel's logical CSS rect plus the window scale factor. Rust positions the
/// subsurface in logical coordinates and tells the engine to render at `width*scale × height*scale`
/// device pixels.
export interface ViewportBounds {
  x: number;
  y: number;
  width: number;
  height: number;
  scale: number;
}

/// Result of a viewport `pick`: the engine tests billboards then mesh AABBs. `id`/`name` are
/// present only on a hit.
export interface PickResult {
  hit: boolean;
  kind?: "mesh" | "billboard" | "vegetation" | "micro-vegetation";
  id?: string;
  name?: string;
  plant?: string;
  position?: [number, number, number];
  normal?: [number, number, number];
}

export interface RecentProject {
  path: string;
  name: string;
  displayName: string;
  lastOpenedAt: string;
}

export interface RecentProjects {
  projects: RecentProject[];
}

export interface AppDataInfo {
  appDataDir: string;
  userdataDir: string;
  envProject: boolean;
  scratchProject: boolean;
}

/// A host session's boot intent: open a project selection, create a fresh project, or empty (the
/// host resolves `SAFFRON_PROJECT` / `SAFFRON_SCRATCH_PROJECT` / a cwd `project.json` from the
/// environment).
export interface SessionIntent {
  path?: string;
  create?: { name: string; displayName: string };
}

/// The `session_status` reply: whether a host is live and the boot intent it started with.
export interface SessionStatus {
  running: boolean;
  path: string;
}

/// The `session-exited` shell event payload: the host exit code, whether the stop was requested
/// (`session_stop`), and the tail of the host log for the crash report.
export interface SessionExited {
  code: number;
  expected: boolean;
  logTail: string[];
}

/// Editor-wide settings persisted in appdata/settings.json. `keyBindings` holds only the user's
/// overrides (command id → key-string); defaults live in lib/keybindings.
export interface EditorSettings {
  keyBindings: Record<string, string>;
}

/// One pointer phase forwarded to the native gizmo. `hover` tracks the handle under the cursor;
/// `begin`/`drag`/`end` bracket a manipulation.
export type GizmoPointerPhase = "hover" | "begin" | "drag" | "end";

/// A spawn preset for `add-entity` (matches the engine's Create menu items).
export type EntityPreset = NonNullable<CommandParamsMap["add-entity"]["preset"]>;
