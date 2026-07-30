import { animationCommands } from "./animation";
import { assetCommands } from "./assets";
import { environmentCommands } from "./environment";
import { projectCommands } from "./project";
import { renderCommands } from "./render";
import { sceneCommands } from "./scene";
import { scriptingCommands } from "./scripting";
import { telemetryCommands } from "./telemetry";
import { vegetationCommands } from "./vegetation";
import { viewportCommands } from "./viewport";

/// The typed control client. Ids are `string` end-to-end (engine Uuids are u64 and can exceed
/// 2^53) — never `Number()` an id.
export const client = {
  ...sceneCommands,
  ...animationCommands,
  ...scriptingCommands,
  ...viewportCommands,
  ...vegetationCommands,
  ...assetCommands,
  ...projectCommands,
  ...telemetryCommands,
  ...environmentCommands,
  ...renderCommands,
};

export type Client = typeof client;

export { ControlError, isBusyLoading } from "./call";
export type {
  AppDataInfo,
  EditorSettings,
  EntityPreset,
  GizmoPointerPhase,
  PickResult,
  ProfilerMode,
  RecentProject,
  RecentProjects,
  ViewId,
  ViewportBounds,
} from "./types";
export type { ProjectInfo, Vec3 } from "../../protocol";
