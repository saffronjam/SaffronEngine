/// The Physics diagnostics panel (a "diagnostics" dock panel beside Stats/Profiler). While
/// Playing it shows the live Jolt world: body/dynamic counts, a per-body table, and a contact /
/// trigger event feed (from the open-AND-playing poll in store.ts). In Edit it shows an empty
/// state and the poll adds zero round-trips. It also hosts the per-selection ragdoll test controls
/// (a designer affordance, like UE's PhAT simulate). It is an INSPECT/TEST surface — gameplay
/// movement (driving a CharacterController) is Lua's job, not an editor button.
import { useEffect, useMemo, useState } from "react";
import { client } from "../control/client";
import { useEditorStore } from "../state/store";
import { errorText, notifyError } from "../lib/flash";
import { SliderField } from "../components/SliderField";
import type { RagdollResult, WorldHitTargetDto } from "../protocol";
import { cn } from "@/lib/utils";
import { Label } from "@/components/ui/label";
import { Button } from "@/components/ui/button";
import { Switch } from "@/components/ui/switch";
import { Separator } from "@/components/ui/separator";
import { ScrollArea } from "@/components/ui/scroll-area";

function Stat({ label, value }: { label: string; value: React.ReactNode }) {
  return (
    <div className="flex items-baseline justify-between gap-2">
      <span className="text-[11px] text-muted-foreground">{label}</span>
      <span className="font-mono text-[11px] tabular-nums text-foreground">{value}</span>
    </div>
  );
}

function SectionLabel({ children }: { children: React.ReactNode }) {
  return (
    <Label className="text-[10px] font-medium tracking-wide text-muted-foreground uppercase">
      {children}
    </Label>
  );
}

function shortId(id: string): string {
  return id.length > 8 ? `…${id.slice(-6)}` : id;
}

export function PhysicsPanel() {
  const playState = useEditorStore((s) => s.playState);
  const playing = playState !== "edit";
  const physicsState = useEditorStore((s) => s.physicsState);
  const physicsBodies = useEditorStore((s) => s.physicsBodies);
  const contactLog = useEditorStore((s) => s.contactLog);
  const contactsOverflowed = useEditorStore((s) => s.contactsOverflowed);
  const selectedId = useEditorStore((s) => s.selectedId);
  const inspected = useEditorStore((s) => s.componentsBySelected);
  const entities = useEditorStore((s) => s.entities);
  const setDragActive = useEditorStore((s) => s.setDragActive);

  const components = inspected?.components as Record<string, Record<string, unknown>> | undefined;
  const hasBonePhysics = !!components && "BonePhysics" in components;

  const nameById = useMemo(() => {
    const m = new Map<string, string>();
    for (const e of entities) {
      m.set(e.id, e.name);
    }
    return m;
  }, [entities]);
  const label = (id: string): string => (id === "0" ? "—" : (nameById.get(id) ?? shortId(id)));
  /// A tagged hit target's display label: the entity's name, or the plant's short id.
  const targetLabel = (target: WorldHitTargetDto | undefined): string => {
    if (!target) return "—";
    if (target.kind === "scene-entity") return label(target.id);
    return `plant ${target.plant.slice(0, 8)}`;
  };
  const targetKey = (target: WorldHitTargetDto | undefined, fallback: string): string => {
    if (!target) return fallback;
    return target.kind === "scene-entity" ? target.id : target.plant;
  };

  // Ragdoll readout: refreshed from get-ragdoll on selection/play change and after each command.
  const [ragdoll, setRagdoll] = useState<RagdollResult | null>(null);
  useEffect(() => {
    if (!playing || !selectedId || !hasBonePhysics) {
      setRagdoll(null);
      return;
    }
    let live = true;
    void client
      .getRagdoll(selectedId)
      .then((r) => {
        if (live) {
          setRagdoll(r);
        }
      })
      .catch(() => {
        if (live) {
          setRagdoll(null);
        }
      });
    return () => {
      live = false;
    };
  }, [playing, selectedId, hasBonePhysics]);

  const onEnableRagdoll = (): void => {
    if (!selectedId) {
      return;
    }
    void client
      .enableRagdoll(selectedId)
      .then(setRagdoll)
      .catch((e) => notifyError(errorText(e)));
  };
  const onRagdollActive = (active: boolean): void => {
    if (!selectedId) {
      return;
    }
    void client
      .setRagdoll({ entity: selectedId, active })
      .then(setRagdoll)
      .catch((e) => notifyError(errorText(e)));
  };
  const onBodyWeight = (bodyWeight: number): void => {
    if (!selectedId) {
      return;
    }
    setRagdoll((r) => (r ? { ...r, bodyWeight } : r)); // optimistic readout
    void client
      .setRagdoll({ entity: selectedId, bodyWeight })
      .then(setRagdoll)
      .catch((e) => notifyError(errorText(e)));
  };

  return (
    <div className="flex h-full min-h-0 flex-col">
      <ScrollArea className="min-h-0 flex-1">
        <div className="flex flex-col gap-3 p-2.5">
          {/* Live world stats */}
          <div className="flex flex-col gap-1.5">
            <SectionLabel>World</SectionLabel>
            {playing && physicsState?.active ? (
              <>
                <Stat label="Bodies" value={physicsState.bodyCount} />
                <Stat label="Dynamic" value={physicsState.dynamicCount} />
              </>
            ) : (
              <p className="py-1 text-[11px] text-muted-foreground italic">
                Enter Play to inspect the physics world.
              </p>
            )}
          </div>

          {/* Live body table */}
          {playing && physicsBodies.length > 0 ? (
            <>
              <Separator />
              <div className="flex flex-col gap-1.5">
                <SectionLabel>Bodies</SectionLabel>
                <div className="flex flex-col gap-0.5">
                  {physicsBodies.map((b, index) => (
                    <div
                      key={targetKey(b.target, `body-${index}`)}
                      className="grid grid-cols-[1fr_auto_auto] items-center gap-2 rounded-sm bg-muted/20 px-1.5 py-0.5 text-[11px]"
                    >
                      <span className="truncate text-foreground">{targetLabel(b.target)}</span>
                      <span className="font-mono text-muted-foreground">{b.motion}</span>
                      <span
                        className={cn(
                          "font-mono text-[10px]",
                          b.active ? "text-emerald-400" : "text-muted-foreground",
                        )}
                      >
                        {b.active ? "awake" : "sleep"}
                      </span>
                    </div>
                  ))}
                </div>
              </div>
            </>
          ) : null}

          {/* Contact / trigger feed */}
          {playing ? (
            <>
              <Separator />
              <div className="flex flex-col gap-1.5">
                <div className="flex items-center justify-between">
                  <SectionLabel>Contacts</SectionLabel>
                  {contactsOverflowed ? (
                    <span className="text-[10px] text-amber-400/90">events dropped</span>
                  ) : null}
                </div>
                {contactLog.length === 0 ? (
                  <p className="py-1 text-[11px] text-muted-foreground italic">No contacts yet.</p>
                ) : (
                  <div className="flex flex-col gap-0.5">
                    {contactLog.map((c) => (
                      <div
                        key={c.seq}
                        className="flex items-center gap-1.5 rounded-sm bg-muted/20 px-1.5 py-0.5 text-[11px]"
                      >
                        <span
                          className={cn(
                            "font-mono",
                            c.kind === "begin" ? "text-emerald-400" : "text-muted-foreground",
                          )}
                        >
                          {c.kind === "begin" ? "+" : "−"}
                        </span>
                        {c.sensor ? (
                          <span className="rounded-sm bg-green-950 px-1 text-[9px] text-green-300">
                            trigger
                          </span>
                        ) : null}
                        <span className="truncate text-foreground">{targetLabel(c.targetA)}</span>
                        <span className="text-muted-foreground">↔</span>
                        <span className="truncate text-foreground">{targetLabel(c.targetB)}</span>
                      </div>
                    ))}
                  </div>
                )}
              </div>
            </>
          ) : null}

          {/* Per-selection ragdoll test controls (rig with BonePhysics) — a designer affordance,
              like UE's PhAT simulate; gameplay ragdoll triggers belong in Lua. */}
          {hasBonePhysics ? (
            <>
              <Separator />
              <div className="flex flex-col gap-1.5">
                <div className="flex items-center justify-between">
                  <SectionLabel>Ragdoll</SectionLabel>
                  {!playing ? (
                    <span className="text-[10px] text-muted-foreground italic">enter Play</span>
                  ) : null}
                </div>
                <div className="flex flex-col gap-1.5">
                  <Button
                    type="button"
                    size="sm"
                    variant="outline"
                    disabled={!playing}
                    onClick={onEnableRagdoll}
                  >
                    Go limp
                  </Button>
                  <div className="flex items-center justify-between gap-2">
                    <Label className="text-[11px] text-muted-foreground">Active (motors)</Label>
                    <Switch
                      checked={ragdoll?.active === true}
                      disabled={!playing}
                      onCheckedChange={onRagdollActive}
                    />
                  </div>
                  <div className="grid grid-cols-[78px_1fr] items-center gap-1.5">
                    <Label className="text-[11px] text-muted-foreground">Physics blend</Label>
                    <SliderField
                      value={ragdoll?.bodyWeight ?? 0}
                      min={0}
                      max={1}
                      step={0.01}
                      onChange={onBodyWeight}
                      onDragStart={() => setDragActive(true)}
                      onDragEnd={() => setDragActive(false)}
                    />
                  </div>
                </div>
                {playing && ragdoll ? (
                  <div className="flex flex-col gap-0.5 pt-0.5">
                    <Stat label="Present" value={ragdoll.present ? "yes" : "no"} />
                    <Stat label="Mean weight" value={ragdoll.bodyWeight.toFixed(2)} />
                    <Stat label="Bones" value={ragdoll.bones} />
                  </div>
                ) : null}
              </div>
            </>
          ) : null}
        </div>
      </ScrollArea>
    </div>
  );
}
