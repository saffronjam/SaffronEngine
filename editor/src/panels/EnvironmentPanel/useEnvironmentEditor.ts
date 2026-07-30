import { useMemo, useRef } from "react";
import { client } from "../../control/client";
import { makeCoalescer, type Coalescer } from "../../control/coalesce";
import { useEditorStore } from "../../state/store";
import { humanizeFieldName } from "@/lib/humanize";
import type { Environment } from "../../protocol";

export type NestedBlock = "atmosphere" | "fog" | "cloud" | "wind" | "timeOfDay";
type BlockId = "env" | NestedBlock;

type BlockPatch = Record<string, unknown>;

/// `set-environment` carries no nested block, so each block has its own merge command. Every write
/// is a server-side merge of the single named field; the merged environment folds back into the
/// store so a clamp or normalize round-trips.
const SEND: Record<BlockId, (patch: BlockPatch) => Promise<Environment>> = {
  env: (patch) => client.setEnvironment(patch as Partial<Environment>),
  atmosphere: (patch) => client.setAtmosphere(patch as Partial<Environment["atmosphere"]>),
  fog: (patch) => client.setFog(patch as Partial<Environment["fog"]>),
  cloud: (patch) => client.setClouds(patch as Partial<Environment["cloud"]>),
  wind: (patch) => client.setWind(patch as Partial<Environment["wind"]>),
  timeOfDay: (patch) => client.setTimeOfDay(patch as Partial<Environment["timeOfDay"]>),
};

/// One scene-tab undo entry per environment field (scene-global, so no selection id). A no-op is
/// dropped; replay re-sends the same merge command.
function recordEdit(block: BlockId, field: string, prior: unknown, after: unknown): void {
  if (JSON.stringify(prior) === JSON.stringify(after)) {
    return;
  }
  const send = SEND[block];
  useEditorStore.getState().pushEdit(
    {
      label: humanizeFieldName(field),
      undo: () => send({ [field]: prior }),
      redo: () => send({ [field]: after }),
    },
    "scene",
  );
}

export interface EnvironmentEditor {
  patch<K extends keyof Environment>(field: K, value: Environment[K]): void;
  patchBlock<B extends NestedBlock, K extends keyof Environment[B]>(
    block: B,
    field: K,
    value: Environment[B][K],
  ): void;
  /// Bracket a drag or scrub: the whole gesture becomes one undo entry, and `dragActive` keeps the
  /// reconcile poll from clobbering the optimistic value mid-scrub.
  onDragStart(): void;
  onDragEnd(): void;
}

/// The panel's one write path. Every edit writes optimistically into the store and pushes the
/// single changed field through a per-field coalescer, so a scrub sends one request per burst
/// instead of one per tick.
export function useEnvironmentEditor(
  env: Environment,
  onCustomized: () => void,
): EnvironmentEditor {
  const setEnvironment = useEditorStore((s) => s.setEnvironment);
  const setDragActive = useEditorStore((s) => s.setDragActive);

  const coalescers = useRef(new Map<string, Coalescer<BlockPatch>>());
  const coalescerFor = useMemo(
    () =>
      (block: BlockId, field: string): Coalescer<BlockPatch> => {
        const key = `${block}.${field}`;
        let coalescer = coalescers.current.get(key);
        if (!coalescer) {
          coalescer = makeCoalescer<BlockPatch>({
            send: async (patch) => {
              const merged = await SEND[block](patch);
              if (!useEditorStore.getState().dragActive) {
                useEditorStore.getState().setEnvironment(merged);
              }
            },
          });
          coalescers.current.set(key, coalescer);
        }
        return coalescer;
      },
    [],
  );

  // A gesture touches exactly one field, captured on its first tick and recorded as one entry at
  // drag end; a discrete edit records inline.
  const gesturing = useRef(false);
  const gesture = useRef<{ block: BlockId; field: string; prior: unknown } | null>(null);

  const noteEdit = (block: BlockId, field: string, prior: unknown, value: unknown): void => {
    onCustomized();
    if (gesturing.current) {
      gesture.current ??= { block, field, prior: structuredClone(prior) };
    } else {
      recordEdit(block, field, structuredClone(prior), structuredClone(value));
    }
  };

  return {
    patch(field, value) {
      noteEdit("env", field, env[field], value);
      setEnvironment({ ...env, [field]: value } as Environment);
      coalescerFor("env", field).push({ [field]: value });
    },
    patchBlock(block, field, value) {
      const current = env[block];
      noteEdit(block, field as string, current[field], value);
      setEnvironment({ ...env, [block]: { ...current, [field]: value } } as Environment);
      coalescerFor(block, field as string).push({ [field]: value });
    },
    onDragStart() {
      setDragActive(true);
      gesturing.current = true;
      gesture.current = null;
    },
    onDragEnd() {
      setDragActive(false);
      gesturing.current = false;
      const pending = gesture.current;
      gesture.current = null;
      const live = useEditorStore.getState().environment;
      if (!pending || !live) {
        return;
      }
      const container =
        pending.block === "env"
          ? (live as unknown as Record<string, unknown>)
          : (live[pending.block] as unknown as Record<string, unknown>);
      recordEdit(
        pending.block,
        pending.field,
        pending.prior,
        structuredClone(container[pending.field]),
      );
    },
  };
}
