import { call } from "./call";
import type { EntityPreset } from "./types";
import type {
  ComponentBody,
  DrainContactsResult,
  EntityList,
  EntityRef,
  InspectResult,
  PhysicsBodiesResult,
  PhysicsStateResult,
  PlayStateResult,
  RagdollResult,
  Selection,
  Transform,
} from "../../protocol";
import type { PickResult } from "./types";

/// Entities, components, physics telemetry, and the play-mode transport.
export const sceneCommands = {
  listEntities(): Promise<EntityList> {
    return call("list-entities");
  },
  inspect(id: string): Promise<InspectResult> {
    return call("inspect", { entity: id });
  },
  getSelection(): Promise<Selection> {
    return call("get-selection");
  },
  selectEntity(id: string): Promise<unknown> {
    return call("select", { entity: id });
  },
  destroyEntity(id: string): Promise<unknown> {
    return call("destroy-entity", { entity: id });
  },
  /// Reparent (engine-authoritative, cycle-guarded); null detaches to root. Ids stay
  /// strings end-to-end (u64 precision).
  setParent(id: string, parentId: string | null): Promise<EntityRef> {
    return call("set-parent", { entity: id, parent: parentId ?? "0" });
  },
  deselect(): Promise<unknown> {
    return call("deselect");
  },
  addEntity(preset: EntityPreset): Promise<EntityRef> {
    return call("add-entity", { preset });
  },
  /// Create an empty named entity; the engine returns its ref (no auto-select).
  createEntity(name: string): Promise<EntityRef> {
    return call("create-entity", { name });
  },
  copyEntity(id: string): Promise<EntityRef> {
    return call("copy-entity", { entity: id });
  },
  /// Set the entity's Name component; echoes the updated ref.
  renameEntity(id: string, name: string): Promise<EntityRef> {
    return call("rename-entity", { entity: id, name });
  },

  /// `smooth` makes the engine animate the fields toward the values (~25ms) instead
  /// of snapping — sent only mid-drag; the release send omits it.
  setTransform(id: string, partial: Partial<Transform>, smooth?: boolean): Promise<unknown> {
    return call("set-transform", { entity: id, ...partial, ...(smooth ? { smooth: true } : {}) });
  },
  addComponent(id: string, component: string): Promise<unknown> {
    return call("add-component", { entity: id, component });
  },
  removeComponent(id: string, component: string): Promise<unknown> {
    return call("remove-component", { entity: id, component });
  },
  setComponent(id: string, component: string, body: ComponentBody): Promise<unknown> {
    return call("set-component", { entity: id, component, json: body });
  },
  setComponentOrder(id: string, components: string[]): Promise<unknown> {
    return call("set-component-order", { entity: id, components });
  },
  /// Merge one field of a component (read-modify-write, server-side). `index` addresses one
  /// element of an array field (e.g. a `MaterialSet` slot): an object `value` merges its keys
  /// into that element, so `{component:"MaterialSet", field:"slots", index, value:{...}}`
  /// edits a single slot. Auto-adds the component (with defaults) if the entity lacks it.
  setComponentField(
    id: string,
    component: string,
    field: string,
    value: unknown,
    index?: number,
  ): Promise<unknown> {
    return call("set-component-field", {
      entity: id,
      component,
      field,
      value,
      ...(index === undefined ? {} : { index }),
    });
  },
  /// Re-fit an entity's Collider shape to its mesh AABB (the substitute for interactive
  /// collider-resize handles); bumps sceneVersion engine-side so the inspector re-reads.
  fitCollider(id: string): Promise<unknown> {
    return call("fit-collider", { entity: id });
  },

  // Physics telemetry + controls. physics-state / drain-contacts are Edit-safe (inactive/empty
  // when the world is null); the ragdoll commands error when null and move-character is inert in
  // Edit, so the panel play-gates them. WireUuid fields cross as decimal strings — never Number().
  physicsState(): Promise<PhysicsStateResult> {
    return call("physics-state");
  },
  physicsBodies(): Promise<PhysicsBodiesResult> {
    return call("physics-bodies");
  },
  drainContacts(since: number): Promise<DrainContactsResult> {
    return call("drain-contacts", { since });
  },
  enableRagdoll(entity: string, enabled?: boolean): Promise<RagdollResult> {
    return call("enable-ragdoll", enabled === undefined ? { entity } : { entity, enabled });
  },
  setRagdoll(p: {
    entity: string;
    active?: boolean;
    bodyWeight?: number;
    bone?: number;
    weight?: number;
  }): Promise<RagdollResult> {
    return call("set-ragdoll", p);
  },
  getRagdoll(entity: string): Promise<RagdollResult> {
    return call("get-ragdoll", { entity });
  },

  pick(u: number, v: number): Promise<PickResult> {
    return call("pick", { u, v });
  },

  /// Enter play mode (from edit) or resume (from paused): the engine duplicates the
  /// scene and cuts to its primary camera. `hasPrimaryCamera:false` → fly-cam fallback.
  play(): Promise<PlayStateResult> {
    return call("play");
  },
  pause(): Promise<PlayStateResult> {
    return call("pause");
  },
  /// Discard the play duplicate and restore the authored scene.
  stop(): Promise<PlayStateResult> {
    return call("stop");
  },
  /// Advance fixed ticks while paused (default 1).
  step(frames?: number): Promise<PlayStateResult> {
    return call("step", frames === undefined ? {} : { frames });
  },
  getPlayState(): Promise<PlayStateResult> {
    return call("get-play-state");
  },
};
