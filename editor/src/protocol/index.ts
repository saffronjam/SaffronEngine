import type {
  AssetEntryDto,
  EnvironmentDto,
  ProjectInfoDto,
  ProjectStatusDto,
  RenderStatsDto,
  SelectionResult,
  ThumbnailResult,
  WireUuid,
} from "./sa-types";

export type * from "./sa-types";

export type Uuid = WireUuid;
export type AssetEntry = AssetEntryDto;
export type ProjectInfo = ProjectInfoDto;
export type ProjectStatus = ProjectStatusDto;
// The codegen inlines enums into the DTOs that use them (no standalone enum exports), so the phase /
// boot-stage unions are derived from the status DTO's fields — the single source of truth.
export type ProjectPhase = ProjectStatusDto["phase"];
export type BootStage = ProjectStatusDto["stage"];
export type RenderStats = RenderStatsDto;
export type Thumbnail = ThumbnailResult;

export type Environment = EnvironmentDto;
export type Selection = SelectionResult;
