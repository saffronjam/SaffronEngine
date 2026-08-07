import { call } from "./call";
import type {
  CreateScriptResult,
  DrainScriptErrorsResult,
  DrainScriptLogsResult,
  GetScriptSchemaResult,
  SetScriptOverrideResult,
} from "../../protocol";

/// Luau script schemas, overrides, and the error/log drains.
export const scriptingCommands = {
  /// Drain contained script errors with seq > since (the cursor mirrors drain-alarms).
  drainScriptErrors(since: number): Promise<DrainScriptErrorsResult> {
    return call("drain-script-errors", { since });
  },
  /// Drain sa.log lines with seq > since (same seq-cursor protocol as drain-script-errors).
  drainScriptLogs(since: number): Promise<DrainScriptLogsResult> {
    return call("drain-script-logs", { since });
  },
  /// A project script's declared fields (path relative to the project src/).
  getScriptSchema(path: string): Promise<GetScriptSchemaResult> {
    return call("get-script-schema", { path });
  },
  /// Create a boilerplate .lua under the project src/; returns the slot path.
  createScript(name: string): Promise<CreateScriptResult> {
    return call("create-script", { name });
  },
  /// Write one per-instance field override onto a Script slot; null clears it.
  setScriptOverride(
    id: string,
    slot: number,
    name: string,
    value: unknown,
  ): Promise<SetScriptOverrideResult> {
    return call("set-script-override", { entity: id, slot, name, value });
  },
};
