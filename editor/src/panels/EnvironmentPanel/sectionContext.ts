import type { Environment } from "../../protocol";
import type { EnvironmentEditor, NestedBlock } from "./useEnvironmentEditor";

/// What every environment section needs: the live values, the write path, the expand/reset
/// plumbing, and whether the "all details" tier is showing.
export interface EnvironmentSectionContext {
  env: Environment;
  defaults: Environment | null;
  allDetails: boolean;
  editor: EnvironmentEditor;
  sectionOpen(id: string): boolean;
  setSectionOpen(id: string, open: boolean): void;
  /// True when a field diverges from the engine's factory default, driving the section's dirty mark.
  isModified(current: unknown, initial: unknown): boolean;
  resetTopLevel(label: string, fields: (keyof Environment)[]): void;
  resetNested<B extends NestedBlock>(
    label: string,
    block: B,
    fields?: (keyof Environment[B])[],
  ): void;
}

export interface SectionProps {
  ctx: EnvironmentSectionContext;
}
