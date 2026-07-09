/// The Script component's Inspector body: an ordered list of script slots (the
/// runtime executes them top-to-bottom), each with assign/reorder/remove plus the
/// script's declared fields rendered as widgets. Field defaults live in the .lua
/// (fetched via get-script-schema); editing a widget writes only that slot's
/// override (set-script-override) — never the default. An overridden field shows
/// a reset affordance that clears it back to the declared default.
import { useEffect, useRef, useState } from "react";
import { ArrowDown, ArrowUp, FileCode, FilePlus2, FolderOpen, RotateCcw, X } from "lucide-react";
import { invoke, open } from "../shell";
import { client } from "../control/client";
import { useEditorStore, withNativeDialog } from "../state/store";
import { errorText, notifyError } from "../lib/flash";
import { makeCoalescer, type Coalescer } from "../control/coalesce";
import { humanizeFieldName } from "@/lib/humanize";
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
import { Switch } from "@/components/ui/switch";
import { Tooltip, TooltipContent, TooltipTrigger } from "@/components/ui/tooltip";
import { NumberDrag } from "./NumberDrag";
import { VectorEditor } from "./VectorEditor";
import type { ScriptFieldDto, ScriptSlot } from "../protocol";

interface SchemaState {
  fields?: ScriptFieldDto[];
  error?: string;
}

const VEC3_AXES = ["x", "y", "z"] as const;

function vecToRecord(value: unknown): Record<string, number> {
  const arr = Array.isArray(value) ? (value as number[]) : [0, 0, 0];
  return { x: arr[0] ?? 0, y: arr[1] ?? 0, z: arr[2] ?? 0 };
}

export function ScriptSlots({ entityId, scripts }: { entityId: string; scripts: ScriptSlot[] }) {
  const project = useEditorStore((s) => s.project);
  const applyOptimisticComponent = useEditorStore((s) => s.applyOptimisticComponent);
  const setDragActive = useEditorStore((s) => s.setDragActive);
  const [schemas, setSchemas] = useState<Record<string, SchemaState>>({});
  const [newScriptOpen, setNewScriptOpen] = useState(false);
  const [newScriptName, setNewScriptName] = useState("");

  // One schema fetch per unique path; a path edit refetches. Errors render inline
  // in the slot (a broken script is an authoring state, not a transient failure).
  useEffect(() => {
    const paths = [...new Set(scripts.map((s) => s.scriptPath).filter((p) => p.length > 0))];
    for (const path of paths) {
      if (schemas[path] !== undefined) {
        continue;
      }
      setSchemas((prev) => ({ ...prev, [path]: {} }));
      client
        .getScriptSchema(path)
        .then((schema) => setSchemas((prev) => ({ ...prev, [path]: { fields: schema.fields } })))
        .catch((err: unknown) =>
          setSchemas((prev) => ({ ...prev, [path]: { error: errorText(err) } })),
        );
    }
  }, [scripts, schemas]);

  // Slot-list edits rewrite the whole component (set-component does not merge).
  const writeSlots = (next: ScriptSlot[]): void => {
    applyOptimisticComponent("Script", { scripts: next });
    void client
      .setComponent(entityId, "Script", { scripts: next })
      .catch((err: unknown) => notifyError(errorText(err)));
  };

  const onAssign = async (slotIndex?: number): Promise<void> => {
    const root = project?.root ?? "";
    const picked = await withNativeDialog(() =>
      open({
        title: "Assign script",
        defaultPath: root ? `${root}/src` : undefined,
        filters: [{ name: "Lua scripts", extensions: ["lua"] }],
        multiple: false,
      }),
    );
    if (typeof picked !== "string") {
      return;
    }
    // Slots store paths relative to <projectRoot>/src/. The dialog returns an
    // absolute path while project.root may be engine-cwd-relative, so match the
    // root's textual tail; any /src/ anywhere under it keeps its subpath.
    const marker = `${root.replace(/^\.\//, "")}/src/`;
    const at = picked.indexOf(marker);
    const fallback = picked.lastIndexOf("/src/");
    let rel: string;
    if (at >= 0) {
      rel = picked.slice(at + marker.length);
    } else if (fallback >= 0) {
      rel = picked.slice(fallback + "/src/".length);
    } else {
      notifyError("Scripts must live under the project's src/ folder");
      return;
    }
    const next = scripts.map((s, i) => (i === slotIndex ? { scriptPath: rel, overrides: {} } : s));
    if (slotIndex === undefined) {
      next.push({ scriptPath: rel, overrides: {} });
    }
    writeSlots(next);
  };

  // New script: the engine writes the class-table boilerplate under src/ and the
  // fresh file lands directly in a new slot — no external editor round-trip.
  const onCreateScript = async (): Promise<void> => {
    const name = newScriptName.trim();
    if (!name) {
      return;
    }
    try {
      const created = await client.createScript(name);
      setNewScriptOpen(false);
      setNewScriptName("");
      writeSlots([...scripts, { scriptPath: created.path, overrides: {} }]);
    } catch (err) {
      notifyError(errorText(err));
    }
  };

  // Jump to this slot's file in VS Code (the Rust command absolutizes the
  // engine-relative project root and prefers the host `code`).
  const onOpenInVsCode = async (scriptPath: string): Promise<void> => {
    if (!project || !scriptPath) {
      return;
    }
    try {
      await invoke("open_in_vscode", { path: `${project.root}/src/${scriptPath}` });
    } catch (err) {
      notifyError(errorText(err));
    }
  };

  const onRemove = (slotIndex: number): void => {
    writeSlots(scripts.filter((_, i) => i !== slotIndex));
  };

  const onMove = (slotIndex: number, dir: -1 | 1): void => {
    const target = slotIndex + dir;
    if (target < 0 || target >= scripts.length) {
      return;
    }
    const next = [...scripts];
    const tmp = next[slotIndex]!;
    next[slotIndex] = next[target]!;
    next[target] = tmp;
    writeSlots(next);
  };

  // Override writes coalesce per (slot,field) so a scrub never floods the socket;
  // the optimistic overlay keeps the widget live between sends.
  const coalescers = useRef(new Map<string, Coalescer<{ value: unknown }>>());
  useEffect(() => {
    coalescers.current.clear();
  }, [entityId]);
  const overrideCoalescer = (slotIndex: number, name: string): Coalescer<{ value: unknown }> => {
    const key = `${slotIndex}.${name}`;
    let c = coalescers.current.get(key);
    if (!c) {
      // Rejections are logged (throttled) by the coalescer — a scrub stream must
      // not toast per dropped frame.
      c = makeCoalescer<{ value: unknown }>({
        send: (latest) => client.setScriptOverride(entityId, slotIndex, name, latest.value),
      });
      coalescers.current.set(key, c);
    }
    return c;
  };

  // Undo capture for a scrubbed/focused override field: the prior value (undefined =
  // "was default") + which (slot, field), recorded as one entry at the gesture's end.
  const overrideGesture = useRef<{ slotIndex: number; name: string; prior: unknown } | null>(null);

  // Record one scene-tab undo entry for an override edit; a no-op is dropped. Clearing
  // an override (null) is the reset-to-default path; undo re-applies the prior value.
  const recordOverrideEdit = (
    slotIndex: number,
    name: string,
    prior: unknown,
    after: unknown,
  ): void => {
    if (JSON.stringify(prior ?? null) === JSON.stringify(after ?? null)) {
      return;
    }
    useEditorStore.getState().pushEdit(
      {
        label: humanizeFieldName(name),
        selectionId: entityId,
        undo: () => client.setScriptOverride(entityId, slotIndex, name, prior ?? null),
        redo: () => client.setScriptOverride(entityId, slotIndex, name, after ?? null),
      },
      "scene",
    );
  };

  const onOverride = (slotIndex: number, name: string, value: unknown): void => {
    const slot = scripts[slotIndex];
    if (!slot) {
      return;
    }
    const prior = slot.overrides[name];
    const overrides = { ...slot.overrides };
    if (value === null) {
      delete overrides[name];
    } else {
      overrides[name] = value;
    }
    const next = scripts.map((s, i) => (i === slotIndex ? { ...s, overrides } : s));
    // A discrete edit (bool toggle, reset, string commit — no active gesture) records
    // one entry; a scrub's ticks are skipped and recorded at its end.
    if (overrideGesture.current === null) {
      recordOverrideEdit(slotIndex, name, prior, value === null ? undefined : value);
    }
    applyOptimisticComponent("Script", { scripts: next });
    overrideCoalescer(slotIndex, name).push({ value });
  };

  // Per-field gesture brackets (scrub drag, or a string field's focus..blur): capture
  // the prior override, gate the poll, and record one entry at the end.
  const onOverrideDragStart = (slotIndex: number, name: string): void => {
    setDragActive(true);
    overrideGesture.current = { slotIndex, name, prior: scripts[slotIndex]?.overrides[name] };
  };
  const onOverrideDragEnd = (slotIndex: number, name: string): void => {
    setDragActive(false);
    const gesture = overrideGesture.current;
    overrideGesture.current = null;
    if (!gesture || gesture.slotIndex !== slotIndex || gesture.name !== name) {
      return;
    }
    const live = useEditorStore.getState().componentsBySelected?.components as
      | Record<string, unknown>
      | undefined;
    const after = (live?.["Script"] as { scripts?: ScriptSlot[] } | undefined)?.scripts?.[slotIndex]
      ?.overrides?.[name];
    recordOverrideEdit(slotIndex, name, gesture.prior, after);
  };
  const fieldDrag = (slotIndex: number, name: string) => ({
    onDragStart: () => onOverrideDragStart(slotIndex, name),
    onDragEnd: () => onOverrideDragEnd(slotIndex, name),
  });

  const renderFieldWidget = (
    slotIndex: number,
    slot: ScriptSlot,
    field: ScriptFieldDto,
  ): React.ReactElement => {
    const overridden = field.name in slot.overrides;
    const value = overridden ? slot.overrides[field.name] : field.defaultValue;
    switch (field.type) {
      case "bool":
        return (
          <Switch
            checked={value === true}
            onCheckedChange={(checked) => onOverride(slotIndex, field.name, checked)}
          />
        );
      case "string":
        return (
          <Input
            type="text"
            className="h-7 rounded-sm bg-background px-1.5 py-0.5 font-mono text-[11px]"
            value={typeof value === "string" ? value : ""}
            onFocus={() => onOverrideDragStart(slotIndex, field.name)}
            onBlur={() => onOverrideDragEnd(slotIndex, field.name)}
            onChange={(event) => onOverride(slotIndex, field.name, event.currentTarget.value)}
          />
        );
      case "vec3": {
        const record = vecToRecord(value);
        return (
          <VectorEditor
            axes={VEC3_AXES}
            value={record}
            onChange={(patch) => {
              const merged = { ...record, ...patch };
              onOverride(slotIndex, field.name, [merged.x, merged.y, merged.z]);
            }}
            {...fieldDrag(slotIndex, field.name)}
          />
        );
      }
      default:
        return (
          <NumberDrag
            value={typeof value === "number" ? value : 0}
            onChange={(v) => onOverride(slotIndex, field.name, v)}
            {...fieldDrag(slotIndex, field.name)}
          />
        );
    }
  };

  return (
    <div className="flex flex-col gap-1.5">
      {scripts.length === 0 ? (
        <p className="px-1 py-0.5 text-[11px] italic text-muted-foreground">No scripts assigned</p>
      ) : null}
      {scripts.map((slot, slotIndex) => {
        const schema = slot.scriptPath ? schemas[slot.scriptPath] : undefined;
        return (
          <div key={slotIndex} className="rounded border border-border/60">
            <div className="flex h-7 items-center gap-0.5 border-b border-border/60 bg-muted/30 pr-0.5 pl-2">
              <button
                type="button"
                className="min-w-0 flex-1 truncate text-left font-mono text-[11px] text-foreground hover:underline"
                onClick={() => void onAssign(slotIndex)}
              >
                {slot.scriptPath || "(unassigned)"}
              </button>
              {slot.scriptPath ? (
                <Tooltip>
                  <TooltipTrigger asChild>
                    <Button
                      type="button"
                      size="icon-xs"
                      variant="ghost"
                      onClick={() => void onOpenInVsCode(slot.scriptPath)}
                    >
                      <FileCode />
                    </Button>
                  </TooltipTrigger>
                  <TooltipContent>Open in VS Code</TooltipContent>
                </Tooltip>
              ) : null}
              <Tooltip>
                <TooltipTrigger asChild>
                  <Button
                    type="button"
                    size="icon-xs"
                    variant="ghost"
                    disabled={slotIndex === 0}
                    onClick={() => onMove(slotIndex, -1)}
                  >
                    <ArrowUp />
                  </Button>
                </TooltipTrigger>
                <TooltipContent>Run earlier (slots run top to bottom)</TooltipContent>
              </Tooltip>
              <Tooltip>
                <TooltipTrigger asChild>
                  <Button
                    type="button"
                    size="icon-xs"
                    variant="ghost"
                    disabled={slotIndex === scripts.length - 1}
                    onClick={() => onMove(slotIndex, 1)}
                  >
                    <ArrowDown />
                  </Button>
                </TooltipTrigger>
                <TooltipContent>Run later (slots run top to bottom)</TooltipContent>
              </Tooltip>
              <Button
                type="button"
                size="icon-xs"
                variant="ghost"
                aria-label="Remove script slot"
                className="text-muted-foreground hover:text-destructive"
                onClick={() => onRemove(slotIndex)}
              >
                <X />
              </Button>
            </div>
            {schema?.error ? (
              <p className="px-2 py-1.5 font-mono text-[11px] whitespace-pre-wrap text-destructive">
                {schema.error}
              </p>
            ) : null}
            {schema?.fields && schema.fields.length > 0 ? (
              <div className="flex flex-col gap-1.5 px-2 py-1.5">
                {schema.fields.map((field) => {
                  const overridden = field.name in slot.overrides;
                  return (
                    <div
                      key={field.name}
                      className="grid grid-cols-[78px_1fr_auto] items-center gap-1.5"
                    >
                      <Label
                        className={`truncate text-[11px] font-normal ${
                          overridden ? "text-foreground" : "text-muted-foreground"
                        }`}
                      >
                        {humanizeFieldName(field.name)}
                      </Label>
                      <div className="min-w-0">{renderFieldWidget(slotIndex, slot, field)}</div>
                      {overridden ? (
                        <Tooltip>
                          <TooltipTrigger asChild>
                            <Button
                              type="button"
                              size="icon-xs"
                              variant="ghost"
                              onClick={() => onOverride(slotIndex, field.name, null)}
                            >
                              <RotateCcw />
                            </Button>
                          </TooltipTrigger>
                          <TooltipContent>Reset to the script's default</TooltipContent>
                        </Tooltip>
                      ) : (
                        <span />
                      )}
                    </div>
                  );
                })}
              </div>
            ) : null}
          </div>
        );
      })}
      <div className="flex gap-1.5">
        <Button
          type="button"
          variant="outline"
          size="sm"
          className="flex-1"
          onClick={() => setNewScriptOpen(true)}
        >
          <FilePlus2 /> New Script
        </Button>
        <Button
          type="button"
          variant="outline"
          size="sm"
          className="flex-1"
          onClick={() => void onAssign()}
        >
          <FolderOpen /> Add Existing
        </Button>
      </div>

      <Dialog open={newScriptOpen} onOpenChange={setNewScriptOpen}>
        <DialogContent className="max-w-sm">
          <DialogHeader>
            <DialogTitle>New script</DialogTitle>
            <DialogDescription>
              Creates a boilerplate .lua under the project's src/ and assigns it to a new slot.
            </DialogDescription>
          </DialogHeader>
          <form
            onSubmit={(event) => {
              event.preventDefault();
              void onCreateScript();
            }}
          >
            <Input
              autoFocus
              type="text"
              placeholder="Script name"
              className="font-mono"
              value={newScriptName}
              onChange={(event) => setNewScriptName(event.currentTarget.value)}
            />
            <DialogFooter className="mt-3">
              <Button type="button" variant="ghost" onClick={() => setNewScriptOpen(false)}>
                Cancel
              </Button>
              <Button type="submit" disabled={!newScriptName.trim()}>
                Create
              </Button>
            </DialogFooter>
          </form>
        </DialogContent>
      </Dialog>
    </div>
  );
}
