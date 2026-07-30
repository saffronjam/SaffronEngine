/// The Inspector panel: registry-driven, with no per-component switch — it renders every present
/// component's fields through `renderField`, so a new engine-side component shows up here
/// automatically.
///
/// Writes are read-modify-write: `set-component` rewrites the whole component, so a single field
/// edit sends the full DTO with that one field patched. `Transform`, uuid fields, and `MaterialSet`
/// slots use the server-merge helpers instead. High-frequency edits funnel through a
/// per-(component, field) coalescer, and the scrub brackets flip `store.dragActive` so the reconcile
/// poll cannot clobber the optimistic value mid-drag.
import { useEffect, useLayoutEffect, useMemo, useRef } from "react";
import { ArrowDownAZ, GripVertical, TriangleAlert, X } from "lucide-react";
import { client } from "../../control/client";
import { useEditorStore } from "../../state/store";
import { makeCoalescer, type Coalescer } from "../../control/coalesce";
import { errorText, notifyError } from "../../lib/flash";
import { renderField, resolveHint } from "../../components/fieldRenderer";
import { ScriptSlots } from "../../components/ScriptSlots";
import { SliderField } from "../../components/SliderField";
import { BoneMaskField } from "../../components/BoneMaskField";
import { FootChainsEditor, type FootChain } from "../../components/FootChainsEditor";
import { BonePhysicsEditor, type BonePhysicsEntry } from "../../components/BonePhysicsEditor";
import type { ScriptSlot, Transform } from "../../protocol";
import { Button } from "@/components/ui/button";
import { Label } from "@/components/ui/label";
import { humanizeComponentName, humanizeFieldName } from "@/lib/humanize";
import { Separator } from "@/components/ui/separator";
import { ScrollArea } from "@/components/ui/scroll-area";
import { Tooltip, TooltipContent, TooltipTrigger } from "@/components/ui/tooltip";
import { logRender } from "../../lib/renderLog";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import { canonicalComponentNames, orderedComponentNames } from "../../lib/componentOrder";
import { MaterialSetSlots } from "./MaterialSetSlots";
import { ADDABLE_COMPONENTS, NON_REMOVABLE, RIG_ONLY } from "./registry";
import { useComponentReorder } from "./useComponentReorder";

/// A non-editable label/value row, matching the field grid's two-column layout. Used by the
/// rig bodies (SkinnedMesh, FootIk, KinematicBones) for import-derived references shown by
/// resolved name rather than as an editable raw-id box.
function ReadonlyRow({ label, value }: { label: string; value: string }) {
  return (
    <div className="grid grid-cols-[78px_1fr] items-center gap-1.5">
      <Label className="truncate text-[11px] font-normal text-muted-foreground">{label}</Label>
      <span className="min-w-0 truncate rounded-sm bg-muted/40 px-1.5 py-1 font-mono text-[11px] text-foreground">
        {value}
      </span>
    </div>
  );
}

export function InspectorPanel() {
  logRender("InspectorPanel");
  const selectedId = useEditorStore((s) => s.selectedId);
  const inspected = useEditorStore((s) => s.componentsBySelected);
  const selectionVersion = useEditorStore((s) => s.selectionVersion);
  const applyOptimisticComponent = useEditorStore((s) => s.applyOptimisticComponent);
  const openMaterialGraphTab = useEditorStore((s) => s.openMaterialGraphTab);
  const focusComponent = useEditorStore((s) => s.focusComponent);
  const setFocusComponent = useEditorStore((s) => s.setFocusComponent);
  // A FogVolume only contributes density to the froxel grid, which runs only while scene fog is
  // enabled in volumetric mode — otherwise the volume renders nothing. Drives the header warning.
  const fog = useEditorStore((s) => s.environment?.fog);
  const fogVolumetric = !!fog?.enabled && fog.mode === "volumetric";
  // Catalog + entity list, used to resolve the read-only id references in the rig bodies
  // (SkinnedMesh mesh/rootBone/joints, FootIk chains, KinematicBones driven) to names.
  const assets = useEditorStore((s) => s.assets);
  const entities = useEditorStore((s) => s.entities);
  const scrollAreaRef = useRef<HTMLDivElement>(null);
  const settleRef = useRef<Map<string, number> | null>(null);

  // Per-(component,field) coalescers, rebuilt when the selection changes so a stale
  // closure never targets the wrong entity.
  const coalescers = useRef(new Map<string, Coalescer<object>>());
  useEffect(() => {
    coalescers.current.clear();
  }, [selectionVersion, selectedId]);

  // The morph-weight coalescer: one `set-morph-weights` per edit-burst (not per scrub tick).
  // `send` reads the live selected id from the store, so the closure never targets a stale
  // entity after a selection change.
  const morphCoalescer = useRef<Coalescer<number[]> | null>(null);
  const morphCoalescerFor = (): Coalescer<number[]> => {
    if (!morphCoalescer.current) {
      morphCoalescer.current = makeCoalescer<number[]>({
        send: (weights) => {
          const id = useEditorStore.getState().selectedId;
          if (!id) {
            return Promise.resolve();
          }
          return client
            .setMorphWeights(id, weights)
            .catch((err: unknown) => notifyError(errorText(err)));
        },
      });
    }
    return morphCoalescer.current;
  };

  // Undo capture: the in-flight field/slot gesture's prior snapshot + target entity,
  // recorded as one undo entry at the gesture's end. Distinct refs so a field scrub and
  // a slot scrub never alias.
  const fieldGesture = useRef<{
    component: string;
    field: string;
    prior: Record<string, unknown>;
    id: string;
  } | null>(null);
  const slotGesture = useRef<{
    slotIndex: number;
    field: string;
    prior: Record<string, unknown>;
    id: string;
  } | null>(null);

  // Consume the one-shot "jump to component" signal from the hierarchy subrows:
  // scroll the section into view when present, and always clear the signal so a
  // stale value never fires on a later render (component absent, selection raced).
  const sectionRefs = useRef(new Map<string, HTMLElement>());
  useLayoutEffect(() => {
    const before = settleRef.current;
    if (!before) {
      return;
    }
    settleRef.current = null;
    const nodes = [...sectionRefs.current.entries()];
    for (const [, node] of nodes) {
      node.style.transition = "none";
    }
    const settles: [HTMLElement, number][] = [];
    for (const [component, node] of nodes) {
      const top = before.get(component);
      if (top === undefined) {
        continue;
      }
      const diff = top - node.getBoundingClientRect().top;
      if (Math.abs(diff) >= 0.5) {
        settles.push([node, diff]);
      }
    }
    for (const [, node] of nodes) {
      node.style.transition = "";
    }
    for (const [node, diff] of settles) {
      node.animate([{ transform: `translateY(${diff}px)` }, { transform: "none" }], {
        duration: 150,
        easing: "ease-out",
      });
    }
  });

  useEffect(() => {
    if (!focusComponent) {
      return;
    }
    sectionRefs.current
      .get(focusComponent)
      ?.scrollIntoView({ block: "nearest", behavior: "smooth" });
    setFocusComponent(null);
  }, [focusComponent, setFocusComponent]);

  const componentsObj = inspected?.components as Record<string, unknown> | undefined;
  const names = useMemo(
    () => (componentsObj ? orderedComponentNames(componentsObj, inspected?.componentOrder) : []),
    [componentsObj, inspected?.componentOrder],
  );
  // The two rig sidecars are meaningless without a skeleton: only offer them to add when
  // the entity carries a SkinnedMesh. (They keep their COMPONENT_ORDER slot so an
  // already-present section still renders — this gates add-availability, not visibility.)
  const hasSkin = !!componentsObj && "SkinnedMesh" in componentsObj;
  const missing = useMemo(
    () =>
      ADDABLE_COMPONENTS.filter(
        (c) => !(componentsObj && c in componentsObj) && (!RIG_ONLY.has(c) || hasSkin),
      ),
    [componentsObj, hasSkin],
  );

  const {
    componentDrag,
    beginComponentDrag,
    moveComponentDrag,
    endComponentDrag,
    resetComponentDrag,
    componentDragStyle,
    onSortComponents,
  } = useComponentReorder({
    names,
    selectedId,
    componentsObj,
    sectionRefs,
    settleRef,
    scrollAreaRef,
  });

  if (!selectedId || !inspected || !componentsObj) {
    return (
      <div className="flex h-full min-h-0 flex-col">
        <div className="min-h-0 flex-1 p-3.5 text-center italic text-muted-foreground">
          No entity selected
        </div>
      </div>
    );
  }

  // Resolve the coalescer for a (component,field) write, building it on first use.
  // The send routes by component to the right merge helper; for full-DTO components
  // the buffered value IS the full DTO (read-modify-write).
  const coalescerFor = (component: string, field: string): Coalescer<object> => {
    const key = `${component}.${field}`;
    let c = coalescers.current.get(key);
    if (!c) {
      c = makeCoalescer<object>({
        send: (latest) => sendWrite(component, field, latest),
      });
      coalescers.current.set(key, c);
    }
    return c;
  };

  // Route a write to the right command for an explicit entity id. `payload` is the full
  // component DTO; the merge helpers (Transform/Material) take the changed field, a uuid
  // field its assign command, everything else the whole DTO (set-component does not
  // merge). Used live by `sendWrite` and by undo/redo replay with a captured id — so a
  // replay always targets the edited entity, never the live selection.
  const applyWrite = (
    id: string,
    component: string,
    field: string,
    payload: object,
    smooth: boolean,
  ): Promise<unknown> => {
    const dto = payload as Record<string, unknown>;
    const hint = resolveHint(component, field, dto[field]);
    if (hint.kind === "uuid") {
      const assetId = String(dto[field] ?? "0");
      if (component === "Mesh" && field === "mesh") {
        return client.assignAsset(id, "mesh", assetId);
      }
      if (component === "Material" && field === "albedoTexture") {
        return client.assignAsset(id, "albedo", assetId);
      }
      if (component === "Material" && field === "metallicRoughnessTexture") {
        return client.assignAsset(id, "metallic-roughness", assetId);
      }
      return client.setComponentField(id, component, field, assetId);
    }
    if (component === "Transform") {
      return client.setTransform(id, { [field]: dto[field] } as Partial<Transform>, smooth);
    }
    return client.setComponent(id, component, dto);
  };

  // The live send for the field coalescer: current selection + drag-smoothing. Mid-drag
  // sends animate toward the value; the post-release re-push goes out exact.
  const sendWrite = (component: string, field: string, payload: object): Promise<unknown> => {
    const id = useEditorStore.getState().selectedId;
    if (!id) {
      return Promise.resolve();
    }
    return applyWrite(id, component, field, payload, useEditorStore.getState().dragActive);
  };

  const setDragActive = useEditorStore.getState().setDragActive;
  const pushEdit = useEditorStore.getState().pushEdit;

  // Record one scene-tab undo entry for a field edit; a no-op (prior === after) is
  // dropped. Undo/redo replay through `applyWrite` against the captured entity id.
  const recordFieldEdit = (
    id: string,
    component: string,
    field: string,
    prior: object,
    after: object,
  ): void => {
    if (JSON.stringify(prior) === JSON.stringify(after)) {
      return;
    }
    pushEdit(
      {
        label: humanizeFieldName(field),
        selectionId: id,
        undo: () => applyWrite(id, component, field, prior, false),
        redo: () => applyWrite(id, component, field, after, false),
      },
      "scene",
    );
  };

  // A field edit: optimistically overlay the patched DTO and push it through the
  // coalescer. A discrete edit (bool/uuid, or a text commit — no active gesture) records
  // one undo entry here; a gesture's ticks are skipped and recorded at its end.
  const onFieldChange = (component: string, field: string, next: unknown): void => {
    const current = (componentsObj[component] ?? {}) as Record<string, unknown>;
    const patched = { ...current, [field]: next };
    if (fieldGesture.current === null) {
      const id = useEditorStore.getState().selectedId;
      if (id) {
        recordFieldEdit(id, component, field, structuredClone(current), structuredClone(patched));
      }
    }
    applyOptimisticComponent(component, patched);
    coalescerFor(component, field).push(patched);
  };

  // A field gesture (scrub drag, or a text field's focus..blur): capture the prior DTO +
  // target entity now and gate the poll; one entry is recorded at the end.
  const onFieldDragStart = (component: string, field: string): void => {
    setDragActive(true);
    const id = useEditorStore.getState().selectedId;
    if (id) {
      const prior = structuredClone((componentsObj[component] ?? {}) as Record<string, unknown>);
      fieldGesture.current = { component, field, prior, id };
    }
  };

  // Release: ungate the poll, re-push the latest optimistic value (one exact, non-smooth
  // write), then record the gesture as a single undo entry. Read from the store, not the
  // render closure — the pointerup listener holds a ctx stale by release.
  const onFieldDragEnd = (component: string, field: string): void => {
    setDragActive(false);
    const components = useEditorStore.getState().componentsBySelected?.components as
      | Record<string, unknown>
      | undefined;
    const current = components?.[component];
    if (current) {
      coalescerFor(component, field).push({ ...(current as object) });
    }
    const gesture = fieldGesture.current;
    fieldGesture.current = null;
    if (gesture && gesture.component === component && gesture.field === field && current) {
      recordFieldEdit(
        gesture.id,
        component,
        field,
        gesture.prior,
        structuredClone(current as Record<string, unknown>),
      );
    }
  };

  // MaterialSet slots: edits route through set-component-field with the slot index (which
  // merges the pushed slot object into slots[slotIndex]), not the generic top-level field
  // machinery — the field lives at slots[i].field, and the registry deserializer reads every
  // slot field so all of them round-trip (no per-field allow-list to drift).
  const slotCoalescerFor = (slotIndex: number, field: string): Coalescer<object> => {
    const key = `MaterialSet#${slotIndex}.${field}`;
    let c = coalescers.current.get(key);
    if (!c) {
      c = makeCoalescer<object>({
        send: (latest) => {
          const id = useEditorStore.getState().selectedId;
          if (!id) {
            return Promise.resolve();
          }
          return client.setComponentField(id, "MaterialSet", "slots", latest, slotIndex);
        },
      });
      coalescers.current.set(key, c);
    }
    return c;
  };

  // Record one scene-tab undo entry for a MaterialSet slot edit (set-component-field with the
  // slot index), replayed against the captured entity id.
  const recordSlotEdit = (
    id: string,
    slotIndex: number,
    field: string,
    prior: Record<string, unknown>,
    after: Record<string, unknown>,
  ): void => {
    if (JSON.stringify(prior) === JSON.stringify(after)) {
      return;
    }
    const apply = (slot: Record<string, unknown>): Promise<unknown> =>
      client.setComponentField(id, "MaterialSet", "slots", { [field]: slot[field] }, slotIndex);
    pushEdit(
      {
        label: humanizeFieldName(field),
        selectionId: id,
        undo: () => apply(prior),
        redo: () => apply(after),
      },
      "scene",
    );
  };

  const onSlotFieldChange = (slotIndex: number, field: string, next: unknown): void => {
    const set = (componentsObj["MaterialSet"] ?? {}) as { slots?: Record<string, unknown>[] };
    const slots = (set.slots ?? []).map((s, i) => (i === slotIndex ? { ...s, [field]: next } : s));
    if (slotGesture.current === null) {
      const id = useEditorStore.getState().selectedId;
      const priorSlot = set.slots?.[slotIndex];
      if (id && priorSlot) {
        recordSlotEdit(
          id,
          slotIndex,
          field,
          structuredClone(priorSlot),
          structuredClone(slots[slotIndex] ?? {}),
        );
      }
    }
    applyOptimisticComponent("MaterialSet", { slots });
    slotCoalescerFor(slotIndex, field).push({ ...(slots[slotIndex] ?? {}) });
  };

  const onSlotFieldDragStart = (slotIndex: number, field: string): void => {
    setDragActive(true);
    const id = useEditorStore.getState().selectedId;
    const slot = ((componentsObj["MaterialSet"] ?? {}) as { slots?: Record<string, unknown>[] })
      .slots?.[slotIndex];
    if (id && slot) {
      slotGesture.current = { slotIndex, field, prior: structuredClone(slot), id };
    }
  };

  const onSlotFieldDragEnd = (slotIndex: number, field: string): void => {
    setDragActive(false);
    const components = useEditorStore.getState().componentsBySelected?.components as
      | Record<string, unknown>
      | undefined;
    const slot = (components?.["MaterialSet"] as { slots?: Record<string, unknown>[] } | undefined)
      ?.slots?.[slotIndex];
    if (slot) {
      slotCoalescerFor(slotIndex, field).push({ ...slot });
    }
    const gesture = slotGesture.current;
    slotGesture.current = null;
    if (gesture && gesture.slotIndex === slotIndex && gesture.field === field && slot) {
      recordSlotEdit(gesture.id, slotIndex, field, gesture.prior, structuredClone(slot));
    }
  };

  // Write one exposed-parameter override onto a slot: merge it into the slot's `overrides`
  // map and push the whole map as the slot's `overrides` field (set-component-field merges
  // it into slots[i]). The referenced material's own value shows through for any key absent.
  const setSlotOverride = (slotIndex: number, field: string, value: unknown): void => {
    const set = (componentsObj["MaterialSet"] ?? {}) as { slots?: Record<string, unknown>[] };
    const prior = (set.slots?.[slotIndex]?.overrides as Record<string, unknown> | undefined) ?? {};
    onSlotFieldChange(slotIndex, "overrides", { ...prior, [field]: value });
  };

  // Clear one override (revert the parameter to the referenced material's value).
  const clearSlotOverride = (slotIndex: number, field: string): void => {
    const set = (componentsObj["MaterialSet"] ?? {}) as { slots?: Record<string, unknown>[] };
    const next = {
      ...((set.slots?.[slotIndex]?.overrides as Record<string, unknown> | undefined) ?? {}),
    };
    delete next[field];
    onSlotFieldChange(slotIndex, "overrides", next);
  };

  // Add/remove a component records its inverse only after the engine accepts it (a
  // rejected op records nothing). Remove captures the full prior body so undo restores
  // the user's values, not engine defaults.
  const onRemove = (component: string): void => {
    const id = selectedId;
    const priorOrder = [...names];
    const afterOrder = names.filter((name) => name !== component);
    const priorBody = structuredClone((componentsObj[component] ?? {}) as Record<string, unknown>);
    void client
      .removeComponent(id, component)
      .then(() => {
        pushEdit(
          {
            label: `Remove ${component}`,
            selectionId: id,
            undo: async () => {
              await client.addComponent(id, component);
              await client.setComponent(id, component, priorBody);
              await client.setComponentOrder(id, priorOrder);
            },
            redo: async () => {
              await client.removeComponent(id, component);
              await client.setComponentOrder(id, afterOrder);
            },
          },
          "scene",
        );
      })
      .catch((err: unknown) => notifyError(errorText(err)));
  };
  const onAdd = (component: string): void => {
    const id = selectedId;
    const afterOrder = [...names, component];
    void client
      .addComponent(id, component)
      .then(() => {
        pushEdit(
          {
            label: `Add ${component}`,
            selectionId: id,
            undo: () => client.removeComponent(id, component),
            redo: async () => {
              await client.addComponent(id, component);
              await client.setComponentOrder(id, afterOrder);
            },
          },
          "scene",
        );
      })
      .catch((err: unknown) => notifyError(errorText(err)));
  };

  // Re-fit the selected Collider to its mesh AABB. The engine bumps sceneVersion, so the
  // reconcile poll re-reads the now-fitted halfExtents/offset/sourceMesh — no optimistic
  // overlay needed. Not undoable (a derived geometry op, like the render-config toggles).
  const onFitCollider = (): void => {
    void client.fitCollider(selectedId).catch((err: unknown) => notifyError(errorText(err)));
  };

  // One section body per component, dispatched by name with early returns (not a
  // JSX ternary chain). Script and MaterialSet have structured slot bodies; Collider and
  // BonePhysics have minimal structured bodies; every other component is the generic grid.
  const componentBody = (component: string, dto: Record<string, unknown>): React.ReactElement => {
    if (component === "Script") {
      return (
        <ScriptSlots
          entityId={selectedId}
          scripts={(dto.scripts as ScriptSlot[] | undefined) ?? []}
        />
      );
    }
    if (component === "Morph") {
      // One 0..1 slider per blend-shape target, labelled by the durable target names.
      // Weights are canonical 0..1 end-to-end (no /100). Each scrub coalesces into one
      // `set-morph-weights` per edit-burst and gates the reconcile poll via `dragActive`.
      const weights = Array.isArray(dto.weights) ? (dto.weights as number[]) : [];
      const names = Array.isArray(dto.names) ? (dto.names as string[]) : [];
      if (weights.length === 0) {
        return <span className="text-[11px] text-muted-foreground">No morph targets.</span>;
      }
      const writeWeight = (k: number, v: number): void => {
        const next = weights.slice();
        next[k] = v;
        applyOptimisticComponent("Morph", { ...dto, weights: next });
        morphCoalescerFor().push(next);
      };
      return (
        <>
          {weights.map((weight, k) => (
            <div key={k} className="grid grid-cols-[78px_1fr] items-center gap-1.5">
              <Label className="truncate text-[11px] font-normal text-muted-foreground">
                {names[k] ?? `Target ${k}`}
              </Label>
              <div className="min-w-0">
                <SliderField
                  value={weight}
                  min={0}
                  max={1}
                  onChange={(v) => writeWeight(k, v)}
                  onDragStart={() => setDragActive(true)}
                  onDragEnd={() => setDragActive(false)}
                />
              </div>
            </div>
          ))}
        </>
      );
    }
    if (component === "MaterialSet") {
      return (
        <MaterialSetSlots
          slots={(dto.slots as Record<string, unknown>[] | undefined) ?? []}
          onSlotFieldChange={onSlotFieldChange}
          onSlotFieldDragStart={onSlotFieldDragStart}
          onSlotFieldDragEnd={onSlotFieldDragEnd}
          setSlotOverride={setSlotOverride}
          clearSlotOverride={clearSlotOverride}
          onEditMaterial={openMaterialGraphTab}
        />
      );
    }
    // The generic field grid, shared by the default body and the Collider body (which
    // prepends an action row above it).
    const fieldGrid = (comp: string, body: Record<string, unknown>): React.ReactElement => (
      <>
        {Object.entries(body).map(([field, value]) => (
          <div key={field} className="grid grid-cols-[78px_1fr] items-center gap-1.5">
            <Label className="truncate text-[11px] font-normal text-muted-foreground">
              {humanizeFieldName(field)}
            </Label>
            <div className="min-w-0">
              {renderField(comp, field, value, (next) => onFieldChange(comp, field, next), {
                onDragStart: () => onFieldDragStart(comp, field),
                onDragEnd: () => onFieldDragEnd(comp, field),
              })}
            </div>
          </div>
        ))}
      </>
    );

    if (component === "Collider") {
      // Auto-fit substitutes for the interactive resize handles Saffron has no gizmo for;
      // a collider with no Rigidbody is a static body (the engine rule) — surface why.
      const isStatic = !("Rigidbody" in componentsObj);
      return (
        <>
          <div className="flex items-center justify-between gap-2 pb-0.5">
            <Button type="button" size="xs" variant="outline" onClick={onFitCollider}>
              Fit to mesh
            </Button>
            {isStatic ? (
              <span className="truncate text-[10px] text-muted-foreground">
                No Rigidbody — static body
              </span>
            ) : null}
          </div>
          {fieldGrid("Collider", dto)}
        </>
      );
    }

    // Resolve the read-only id references in the rig bodies to display names: mesh ids hit the
    // asset catalog; rootBone/bone ids are scene entities (joints carry a Name); foot-IK and
    // kinematic-bone arrays hold integer indices into the rig's SkinnedMesh.bones.
    const assetName = (id: unknown): string => {
      const s = String(id ?? "0");
      if (s === "0" || s === "") {
        return "(none)";
      }
      return assets.find((a) => a.id === s)?.name ?? s;
    };
    const entityName = (id: unknown): string => {
      const s = String(id ?? "0");
      if (s === "0" || s === "") {
        return "(none)";
      }
      return entities.find((e) => e.id === s)?.name ?? s;
    };
    const rigBones = (): string[] => {
      const skin = componentsObj["SkinnedMesh"] as { bones?: unknown } | undefined;
      const bones = skin && Array.isArray(skin.bones) ? (skin.bones as unknown[]) : [];
      return bones.map((b) => String(b));
    };
    // The rig's joints as {index, id (bone uuid), name} for the bone pickers and per-bone cards.
    const rigJoints = (): { index: number; id: string; name: string }[] =>
      rigBones().map((id, index) => ({
        index,
        id,
        name: entities.find((e) => e.id === id)?.name ?? `Joint ${index}`,
      }));

    if (component === "SkinnedMesh") {
      // Import-derived rig data is read-only: the bone uuid array and the inverse-bind matrices
      // are never hand-edited (and the matrices are a meaningless JSON blob), so show a resolved
      // mesh / root-bone / joint-count summary instead of a field grid.
      const joints = rigBones();
      return (
        <div className="flex flex-col gap-1.5">
          <ReadonlyRow label="Mesh" value={assetName(dto.mesh)} />
          <ReadonlyRow label="Root bone" value={entityName(dto.rootBone)} />
          <ReadonlyRow
            label="Joints"
            value={`${joints.length} ${joints.length === 1 ? "joint" : "joints"} (import order)`}
          />
          <span className="px-0.5 text-[10px] text-muted-foreground">
            Skeleton bindings are set on import and not editable here.
          </span>
        </div>
      );
    }

    if (component === "FootIk") {
      // enabled/groundHeight are scalars (the generic grid); chains[] is the two-bone IK limbs,
      // edited as add/remove cards with joints picked by name. Writes round-trip the whole FootIk
      // DTO through set-component (consumed by the animation evaluator on the next frame in Play).
      const chains = Array.isArray(dto.chains) ? (dto.chains as FootChain[]) : [];
      return (
        <div className="flex flex-col gap-1.5">
          {fieldGrid("FootIk", { enabled: dto.enabled, groundHeight: dto.groundHeight })}
          <FootChainsEditor
            chains={chains}
            joints={rigJoints()}
            onChange={(next) => onFieldChange("FootIk", "chains", next)}
            onDragStart={() => onFieldDragStart("FootIk", "chains")}
            onDragEnd={() => onFieldDragEnd("FootIk", "chains")}
          />
        </div>
      );
    }

    if (component === "KinematicBones") {
      // enabled is the toggle; driven[] is the joint subset that gets kinematic bodies (empty = all),
      // edited as a bone-mask. Built from this array at the next play edge.
      const driven = Array.isArray(dto.driven)
        ? (dto.driven as unknown[]).filter((v): v is number => typeof v === "number")
        : [];
      return (
        <div className="flex flex-col gap-1.5">
          {fieldGrid("KinematicBones", { enabled: dto.enabled })}
          <BoneMaskField
            value={driven}
            joints={rigJoints()}
            onChange={(next) => onFieldChange("KinematicBones", "driven", next)}
          />
        </div>
      );
    }

    if (component === "BonePhysics") {
      // Per-bone ragdoll/collision data, 1:1 with the skeleton: fixed-length cards labeled by joint
      // name. Edits round-trip the whole bones[] through set-component and apply when physics builds
      // at the next play edge.
      const bones = Array.isArray(dto.bones) ? (dto.bones as BonePhysicsEntry[]) : [];
      return (
        <BonePhysicsEditor
          bones={bones}
          joints={rigJoints()}
          onChange={(next) => onFieldChange("BonePhysics", "bones", next)}
          onDragStart={() => onFieldDragStart("BonePhysics", "bones")}
          onDragEnd={() => onFieldDragEnd("BonePhysics", "bones")}
        />
      );
    }

    return fieldGrid(component, dto);
  };

  return (
    <div className="flex h-full min-h-0 flex-col">
      <ScrollArea ref={scrollAreaRef} className="min-h-0 flex-1">
        <div className="flex flex-col gap-2 p-1.5">
          {names.map((component) => {
            const dto = (componentsObj[component] ?? {}) as Record<string, unknown>;
            const removable = !NON_REMOVABLE.has(component);
            const dragged = componentDrag?.dragging && componentDrag.id === component;
            return (
              <section
                key={component}
                ref={(el) => {
                  if (el) {
                    sectionRefs.current.set(component, el);
                  } else {
                    sectionRefs.current.delete(component);
                  }
                }}
                className={[
                  "overflow-hidden rounded-md border border-border bg-background transition-[transform,border-color,box-shadow] duration-150 ease-out",
                  dragged ? "relative z-10 shadow-lg transition-none" : "",
                ].join(" ")}
                style={componentDragStyle(component)}
              >
                <header className="flex h-8 items-center justify-between border-b border-border bg-muted/50 pr-1 pl-1">
                  <div className="flex min-w-0 items-center gap-1">
                    <Tooltip>
                      <TooltipTrigger asChild>
                        <Button
                          type="button"
                          size="icon-xs"
                          variant="ghost"
                          className="cursor-grab text-muted-foreground active:cursor-grabbing"
                          onPointerDown={(event) => beginComponentDrag(component, event)}
                          onPointerMove={moveComponentDrag}
                          onPointerUp={(event) => endComponentDrag(component, event)}
                          onPointerCancel={resetComponentDrag}
                          onLostPointerCapture={resetComponentDrag}
                        >
                          <GripVertical />
                        </Button>
                      </TooltipTrigger>
                      <TooltipContent>Reorder {component}</TooltipContent>
                    </Tooltip>
                    <span className="truncate text-xs font-semibold tracking-wide text-foreground">
                      {humanizeComponentName(component)}
                    </span>
                    {component === "FogVolume" && !fogVolumetric ? (
                      <Tooltip>
                        <TooltipTrigger asChild>
                          <TriangleAlert className="size-3.5 shrink-0 text-amber-400" />
                        </TooltipTrigger>
                        <TooltipContent>
                          Inactive — enable Fog (Volumetric) in the Environment panel.
                        </TooltipContent>
                      </Tooltip>
                    ) : null}
                  </div>
                  {removable ? (
                    <Tooltip>
                      <TooltipTrigger asChild>
                        <Button
                          type="button"
                          size="icon-xs"
                          variant="ghost"
                          className="text-muted-foreground hover:text-destructive"
                          onClick={() => onRemove(component)}
                        >
                          <X />
                        </Button>
                      </TooltipTrigger>
                      <TooltipContent>Remove {component}</TooltipContent>
                    </Tooltip>
                  ) : null}
                </header>
                <div className="flex flex-col gap-1.5 px-2 py-1.5">
                  {componentBody(component, dto)}
                </div>
              </section>
            );
          })}

          <Separator className="my-1" />

          <div className="flex items-center gap-1.5">
            <DropdownMenu>
              <DropdownMenuTrigger asChild>
                <Button
                  type="button"
                  variant="outline"
                  size="sm"
                  className="min-w-0 flex-1"
                  disabled={missing.length === 0}
                >
                  Add Component
                </Button>
              </DropdownMenuTrigger>
              <DropdownMenuContent
                align="start"
                className="w-(--radix-dropdown-menu-trigger-width)"
              >
                {missing.map((component) => (
                  <DropdownMenuItem key={component} onSelect={() => onAdd(component)}>
                    {humanizeComponentName(component)}
                  </DropdownMenuItem>
                ))}
              </DropdownMenuContent>
            </DropdownMenu>
            <Tooltip>
              <TooltipTrigger asChild>
                <Button
                  type="button"
                  variant="outline"
                  size="icon-sm"
                  className="flex-none"
                  onClick={onSortComponents}
                  disabled={
                    JSON.stringify(names) === JSON.stringify(canonicalComponentNames(componentsObj))
                  }
                >
                  <ArrowDownAZ />
                </Button>
              </TooltipTrigger>
              <TooltipContent>Sort components</TooltipContent>
            </Tooltip>
          </div>
        </div>
      </ScrollArea>
    </div>
  );
}
