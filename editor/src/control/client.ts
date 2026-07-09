/// Typed control client. Every engine call goes through the ONE generic Rust
/// passthrough `invoke('control', { cmd, params })`; the Rust layer already turns
/// an engine `ok:false` into a rejected promise, so `call` only narrows the
/// resolve type. Window-handle lifecycle commands (start/attach/resize/quit/alive)
/// have dedicated Rust commands and get thin wrappers.
///
/// Ids are `string` end-to-end (engine Uuids are u64 and can exceed 2^53). NEVER
/// `Number()` an id.
import { invoke } from "../shell";
import type { MaterialGraph } from "../materials/graph";
import type {
  ActiveAlarmsDto,
  AnimationStateResult,
  AssetList,
  AssetUsagesResult,
  CaptureStartParams,
  CaptureStartResult,
  CaptureStatusResult,
  CaptureStopResult,
  PhysicsStateResult,
  PhysicsBodiesResult,
  DrainContactsResult,
  RagdollResult,
  ComponentBody,
  CommandParamsMap,
  CommandResultMap,
  CreateScriptResult,
  DrainAlarmsResult,
  DrainScriptErrorsResult,
  DrainScriptLogsResult,
  EditorCamera,
  AssetPreviewResult,
  GetScriptSchemaResult,
  SetScriptOverrideResult,
  EntityList,
  EntityRef,
  Environment,
  FrameHistoryDto,
  GizmoState,
  InspectResult,
  ListClipsResult,
  ClipBindingsResult,
  MorphWeightsResult,
  PerfConfigDto,
  PlayStateResult,
  ProfilerModeResult,
  RenderPassTimingsDto,
  RenderQualityResult,
  RenderStats,
  AssetPreviewOptionsResult,
  AssetModelResult,
  DebugOverlaysResult,
  PickSkeletonJointResult,
  Selection,
  SetPerfConfigParams,
  GetUpscaleResult,
  SetUpscaleParams,
  SetUpscaleResult,
  SkeletonOverlayResult,
  Thumbnail,
  Transform,
  Vec3,
  ProjectInfo,
  ProjectStatus,
  AppManifest,
  ExportAppResult,
} from "../protocol";

/// The GPU profiler depth (off keeps the present-only host at baseline cost).
export type ProfilerMode = ProfilerModeResult["mode"];

/// Which viewport a per-view command targets. Each view has its own render target + shm ring +
/// subsurface; the wire tokens MUST match the engine's `viewIdFromWire` ("scene" | "assetPreview").
export type ViewId = "scene" | "assetPreview";

/// The viewport panel's logical CSS rect plus the window scale factor. Rust positions
/// the wayland subsurface in logical coordinates and tells the engine to render at
/// `width*scale × height*scale` device pixels.
export interface ViewportBounds {
  x: number;
  y: number;
  width: number;
  height: number;
  scale: number;
}

/// Result of a viewport `pick`: the engine tests billboards then mesh AABBs and
/// reports what (if anything) the ray hit. `id`/`name` are present only on a hit.
export interface PickResult {
  hit: boolean;
  kind?: "mesh" | "billboard";
  id?: string;
  name?: string;
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

/// Editor-wide settings persisted in appdata/settings.json. `keyBindings` holds only
/// the user's overrides (command id → key-string); defaults live in lib/keybindings.
export interface EditorSettings {
  keyBindings: Record<string, string>;
}

/// One pointer phase forwarded to the native gizmo. `hover` tracks the handle
/// under the cursor; `begin`/`drag`/`end` bracket a manipulation.
export type GizmoPointerPhase = "hover" | "begin" | "drag" | "end";

/// A spawn preset for `add-entity` (matches the engine's Create menu items).
export type EntityPreset = NonNullable<CommandParamsMap["add-entity"]["preset"]>;

type CommandName = keyof CommandParamsMap & keyof CommandResultMap;
type EmptyCommandName = {
  [C in CommandName]: keyof CommandParamsMap[C] extends never ? C : never;
}[CommandName];

/// A rejected control call, carrying the engine's machine-readable `code` (present on every
/// `ok:false` reply) alongside the human message. The Rust bridge rejects the `control`
/// passthrough with a `{ message, code }` object; `call()` normalizes it to this one Error type so
/// callers can `isBusyLoading(err)` and `errorText(err)` uniformly.
export class ControlError extends Error {
  code?: string;
  constructor(message: string, code?: string) {
    super(message);
    this.name = "ControlError";
    this.code = code;
  }
}

/// True when a call was discarded because a project load is in flight (the dispatch gate's
/// `busy-loading` code). Background poll lanes drop these silently and retry once the load settles.
export function isBusyLoading(err: unknown): boolean {
  return err instanceof ControlError && err.code === "busy-loading";
}

/// Coerce whatever `invoke` rejected with (the serialized bridge `{ message, code }`, or a bare
/// string when the bridge layer itself failed) into a `ControlError`.
function toControlError(raw: unknown): ControlError {
  if (raw instanceof ControlError) {
    return raw;
  }
  if (raw && typeof raw === "object" && "message" in raw) {
    const obj = raw as { message?: unknown; code?: unknown };
    return new ControlError(
      typeof obj.message === "string" ? obj.message : String(raw),
      typeof obj.code === "string" ? obj.code : undefined,
    );
  }
  return new ControlError(typeof raw === "string" ? raw : String(raw));
}

function call<C extends EmptyCommandName>(cmd: C): Promise<CommandResultMap[C]>;
function call<C extends CommandName>(
  cmd: C,
  params: CommandParamsMap[C],
): Promise<CommandResultMap[C]>;
/// Typed passthrough: resolves with the command's declared result type; rejects with a
/// [`ControlError`] carrying the engine's `code`.
async function call<C extends CommandName>(
  cmd: C,
  params?: CommandParamsMap[C],
): Promise<CommandResultMap[C]> {
  try {
    return await invoke<CommandResultMap[C]>("control", { cmd, params: params ?? {} });
  } catch (raw) {
    throw toControlError(raw);
  }
}

export const client = {
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

  /// The animation clips in the project catalog (global; the catalog is not per-entity).
  listClips(): Promise<ListClipsResult> {
    return call("list-clips", {});
  },
  /// The entity's playhead, clip, wrap, speed, and the animationVersion stamp.
  getAnimationState(entity: string): Promise<AnimationStateResult> {
    return call("get-animation-state", { entity });
  },
  /// Play a clip (previews in Edit too); `blend` cross-fades/inertializes from the current clip,
  /// `paused` loads it at frame 0 without playing (the clip-list pick semantics).
  playAnimation(
    entity: string,
    clip: string,
    opts?: { speed?: number; loop?: boolean; blend?: number; paused?: boolean },
  ): Promise<AnimationStateResult> {
    return call("play-animation", { entity, clip, ...opts });
  },
  /// Resume (playing=true) or pause (playing=false) without moving the playhead — the play/pause
  /// toggle. Distinct from play-animation, which loads a clip and restarts it at frame 0.
  setAnimationPlaying(entity: string, playing: boolean): Promise<AnimationStateResult> {
    return call("set-animation-playing", { entity, playing });
  },
  /// Set the playhead (previews in Edit). Works in Play, Paused, and Edit-preview alike. `seekBlend`
  /// eases the pose to the seeked time over that many seconds instead of snapping (smooth scrubbing).
  seekAnimation(
    entity: string,
    time: number,
    opts?: { seekBlend?: number },
  ): Promise<AnimationStateResult> {
    return call("seek-animation", { entity, time, ...opts });
  },
  setAnimationLoop(
    entity: string,
    wrap: "once" | "loop" | "pingpong",
  ): Promise<AnimationStateResult> {
    return call("set-animation-loop", { entity, wrap });
  },
  /// Clear the Edit preview and stop, reverting the entity to its rest pose.
  stopPreview(entity: string): Promise<AnimationStateResult> {
    return call("stop-preview", { entity });
  },

  /// Set a morph mesh's blend-shape weights (canonical 0..1; the length must match the
  /// mesh's target count). Writes the runtime override when a preview is live, else the
  /// durable component.
  setMorphWeights(entity: string, weights: number[]): Promise<MorphWeightsResult> {
    return call("set-morph-weights", { entity, weights });
  },
  /// A morph mesh's live blend-shape weights (override-or-component) + the durable target names.
  getMorphWeights(entity: string): Promise<MorphWeightsResult> {
    return call("get-morph-weights", { entity });
  },
  /// A clip's channels resolved against the entity's live forest — node labels resolve to the
  /// bound entity name (raw glTF name on a broken binding); morph labels are the raw target name.
  listClipBindings(entity: string, clip: string): Promise<ClipBindingsResult> {
    return call("list-clip-bindings", { entity, clip });
  },

  /// A model's capabilities + bone tree + clips, read from its .smodel container. Accepts the model, a
  /// mesh sub-asset, or a clip sub-asset — all resolve to the same owning container. A static model
  /// returns an empty bone tree (capabilities.hasRig === false), not an error.
  getAssetModel(asset: string): Promise<AssetModelResult> {
    return call("get-asset-model", { asset });
  },
  /// Open any model in the isolated preview scene; returns the spawned root entity + bone table (empty
  /// for a static model).
  enterAssetPreview(asset: string): Promise<AssetPreviewResult> {
    return call("enter-asset-preview", { asset });
  },
  /// Close the asset preview and restore the authored scene + camera.
  exitAssetPreview(): Promise<PlayStateResult> {
    return call("exit-asset-preview");
  },
  /// Select which view the engine renders + addresses this frame (the rendered pane: scene vs asset
  /// preview). Routes activeScene/camera + the per-view render target; sent when the active tab changes.
  setActiveView(view: ViewId): Promise<CommandResultMap["set-active-view"]> {
    return call("set-active-view", { view });
  },
  /// The previewed model's line-skeleton overlay toggles (master show, per-joint axes, joint size).
  setSkeletonOverlay(opts: {
    show?: boolean;
    axes?: boolean;
    jointSize?: number;
  }): Promise<SkeletonOverlayResult> {
    return call("set-skeleton-overlay", opts);
  },
  /// The viewport debug-overlay toggles (bounds / scene AABB / light volumes).
  getDebugOverlays(): Promise<DebugOverlaysResult> {
    return call("get-debug-overlays", {});
  },
  setDebugOverlays(opts: {
    bounds?: boolean;
    sceneAabb?: boolean;
    lightVolumes?: boolean;
    grid?: boolean;
    colliders?: boolean;
  }): Promise<DebugOverlaysResult> {
    return call("set-debug-overlays", opts);
  },
  /// Tint a previewed model's joint by its get-asset-model node index (-1 clears the highlight).
  setSkeletonHighlight(joint: number): Promise<SkeletonOverlayResult> {
    return call("set-skeleton-highlight", { joint });
  },
  /// Pick the previewed model's nearest joint to a viewport click at normalized (u,v), within radiusPx
  /// pixels. Returns the joint's get-asset-model node index, or found=false when none is close enough.
  pickSkeletonJoint(u: number, v: number, radiusPx?: number): Promise<PickSkeletonJointResult> {
    return call("pick-skeleton-joint", { u, v, radiusPx });
  },
  /// Preview-scene settings (v1: show floor).
  setAssetPreviewOptions(opts: { floor?: boolean }): Promise<AssetPreviewOptionsResult> {
    return call("set-asset-preview-options", opts);
  },

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

  getGizmo(): Promise<GizmoState> {
    return call("get-gizmo");
  },
  setGizmo(state: Partial<GizmoState>): Promise<GizmoState> {
    return call("set-gizmo", state);
  },
  /// Forward one pointer phase to the engine's native gizmo. `x`/`y` are NDC in
  /// [-1, 1] (same `u*2-1` mapping `pick` uses).
  gizmoPointer(phase: GizmoPointerPhase, x: number, y: number): Promise<unknown> {
    return call("gizmo-pointer", { phase, x, y });
  },

  /// Aim the editor camera at the entity (the F shortcut / hierarchy Focus).
  focus(id: string): Promise<EntityRef> {
    return call("focus", { entity: id });
  },
  getCamera(): Promise<EditorCamera> {
    return call("get-camera");
  },
  /// A free-eye set that snaps to the pose (scripting / absolute framing).
  setCamera(camera: Partial<EditorCamera>): Promise<EditorCamera> {
    return call("set-camera", camera);
  },
  /// Drive the preview orbit camera: the engine eases the pivot / distance / angles toward this
  /// each rendered frame and sweeps the eye along the arc, so a fast drag follows the circle
  /// (never chords across it) and the ~60 Hz samples become continuous motion at render FPS.
  setOrbit(orbit: {
    pivot: Vec3;
    distance: number;
    yaw: number;
    pitch: number;
  }): Promise<EditorCamera> {
    return call("set-camera", orbit);
  },

  listAssets(): Promise<AssetList> {
    return call("list-assets");
  },
  getThumbnail(id: string, size?: number): Promise<Thumbnail> {
    return call("get-thumbnail", size === undefined ? { asset: id } : { asset: id, size });
  },
  /// 512-px preview for the View modal (same readback path as get-thumbnail).
  viewAsset(id: string, size?: number): Promise<Thumbnail> {
    return call("view-asset", size === undefined ? { asset: id } : { asset: id, size });
  },
  /// Rename a catalog entry. Returns the new {id, name}.
  renameAsset(id: string, newName: string): Promise<{ id: string; name: string }> {
    return call("rename-asset", { asset: id, name: newName });
  },
  createAssetFolder(folder: string): Promise<AssetList> {
    return call("create-asset-folder", { folder });
  },
  renameAssetFolder(folder: string, name: string): Promise<AssetList> {
    return call("rename-asset-folder", { folder, name });
  },
  deleteAssetFolder(folder: string): Promise<AssetList> {
    return call("delete-asset-folder", { folder });
  },
  moveAsset(
    id: string,
    folder: string | null,
  ): Promise<{ id: string; name: string; folder?: string }> {
    const params: CommandParamsMap["move-asset"] = { asset: id };
    if (folder) {
      params.folder = folder;
    }
    return call("move-asset", params);
  },
  assetUsages(id: string): Promise<AssetUsagesResult> {
    return call("asset-usages", { asset: id });
  },
  /// On-disk metadata for one asset: size, vertex/triangle counts (meshes), and the
  /// file's modified time. Backs the assets-panel detail view.
  probeAsset(id: string): Promise<CommandResultMap["probe-asset"]> {
    return call("probe-asset", { asset: id });
  },
  deleteAsset(id: string): Promise<CommandResultMap["delete-asset"]> {
    return call("delete-asset", { asset: id });
  },
  /// Assign a mesh or albedo texture to an entity slot (adds the component if
  /// missing). The dedicated, minimal write for Mesh.mesh / Material.albedoTexture.
  assignAsset(
    entity: string,
    slot: "mesh" | "albedo" | "metallic-roughness",
    asset: string,
  ): Promise<unknown> {
    return call("assign-asset", { entity, slot, asset });
  },
  /// Create a new default material asset; returns its id + name.
  materialCreate(name: string) {
    return call("material-create", { name });
  },
  /// List the project's material assets (for the material browser/picker).
  materialList() {
    return call("material-list");
  },
  /// Read a material asset's fields (blend/unlit/factors/texture ids).
  materialGet(material: string) {
    return call("material-get", { material });
  },
  /// Edit a material asset's scalar factors in place.
  materialUpdate(
    material: string,
    patch: {
      baseColor?: { x: number; y: number; z: number; w: number };
      metallic?: number;
      roughness?: number;
      emissive?: { x: number; y: number; z: number };
      emissiveStrength?: number;
      normalStrength?: number;
      heightScale?: number;
      heightMode?: string;
      albedoTexture?: string;
      ormTexture?: string;
      normalTexture?: string;
      emissiveTexture?: string;
      heightTexture?: string;
      vectorDisplacementTexture?: string;
    },
  ): Promise<unknown> {
    return call("material-update", { material, ...patch });
  },
  /// Point every `MaterialSet` slot of an entity at a `.smat` material asset ("0" clears to
  /// the built-in default).
  materialAssign(entity: string, material: string): Promise<unknown> {
    return call("material-assign", { entity, material });
  },
  /// Import a folder of PBR textures, suffix-detecting roles, into a new .smat.
  materialImport(path: string, name?: string) {
    return call("material-import", { path, name: name ?? "" });
  },
  /// Render a material on a studio-lit sphere; returns a base64 PNG.
  previewRender(material: string, size?: number) {
    return call("preview-render", { material, size });
  },
  /// Replace a material's node graph; returns whether it folded entirely to params (no codegen node).
  materialSetGraph(material: string, graph: MaterialGraph) {
    return call("material-set-graph", { material, graph });
  },
  /// Compile a material's node graph to a shader via codegen; returns { id, ok }.
  materialCompileGraph(material: string) {
    return call("material-compile-graph", { material });
  },
  /// Import a model from a filesystem path: bakes one .smodel asset + catalog rows and returns the
  /// model asset ref. instantiateModel places it into the scene.
  importModel(path: string): Promise<CommandResultMap["import-model"]> {
    return call("import-model", { path });
  },
  /// Import a texture from a filesystem path into the catalog.
  importTexture(path: string): Promise<{ texture: string }> {
    return call("import-texture", { path });
  },
  /// Expand a model asset's stored hierarchy into the scene; returns the new root entity.
  instantiateModel(asset: string, name?: string): Promise<EntityRef> {
    return name === undefined
      ? call("instantiate-model", { asset })
      : call("instantiate-model", { asset, name });
  },
  /// Preview, commit, or clear a model asset placement driven by viewport drag/drop.
  previewAssetPlacement(
    asset: string,
    u: number,
    v: number,
  ): Promise<CommandResultMap["asset-placement"]> {
    return call("asset-placement", { phase: "preview", asset, u, v });
  },
  commitAssetPlacement(): Promise<CommandResultMap["asset-placement"]> {
    return call("asset-placement", { phase: "commit" });
  },
  clearAssetPlacement(): Promise<CommandResultMap["asset-placement"]> {
    return call("asset-placement", { phase: "clear" });
  },
  /// Rescan assets/ and reconcile the catalog from disk; returns added/removed counts.
  scanAssets(): Promise<CommandResultMap["scan-assets"]> {
    return call("scan-assets");
  },
  /// Slice an embedded sub-asset to a standalone file (keeping its id) + remap the container.
  extractSubAsset(
    asset: string,
    subAsset: string,
    dest?: string,
  ): Promise<CommandResultMap["extract-subasset"]> {
    return dest === undefined
      ? call("extract-subasset", { asset, subAsset })
      : call("extract-subasset", { asset, subAsset, dest });
  },
  /// Revert an extracted sub-asset to the embedded chunk.
  clearExtraction(asset: string, subAsset: string): Promise<CommandResultMap["clear-extraction"]> {
    return call("clear-extraction", { asset, subAsset });
  },
  /// Re-bake a model from its source (skip if unchanged), preserving extractions.
  reimportModel(asset: string): Promise<CommandResultMap["reimport-model"]> {
    return call("reimport-model", { asset });
  },
  /// A container's sub-assets, source recipe, and byte footprint.
  modelInfo(asset: string): Promise<CommandResultMap["model-info"]> {
    return call("model-info", { asset });
  },
  /// What references this / what this references + footprint.
  assetReferences(asset: string): Promise<CommandResultMap["asset-references"]> {
    return call("asset-references", { asset });
  },
  /// A categorized cleanup report (dry-run; never deletes).
  cleanAssets(exclude?: string[]): Promise<CommandResultMap["clean-assets"]> {
    return exclude === undefined ? call("clean-assets", {}) : call("clean-assets", { exclude });
  },
  /// Delete confirmed-unused assets (requires confirm), then rescan.
  deleteUnused(ids: string[], confirm: boolean): Promise<CommandResultMap["delete-unused"]> {
    return call("delete-unused", { ids, confirm });
  },

  getProject(): Promise<ProjectInfo> {
    return call("get-project");
  },
  /// The live project-load phase + boot stage + progress. Allow-listed during `Loading`, so the
  /// loading-screen poll is always answered. See [`isBusyLoading`] for the discarded-command case.
  projectStatus(): Promise<ProjectStatus> {
    return call("project-status");
  },
  /// Abort the in-flight project load; the loader resets to `Unloaded` at its next step.
  cancelLoad(): Promise<ProjectStatus> {
    return call("cancel-load");
  },
  /// Kick off a non-blocking project create — returns the initial `Loading` status snapshot, not
  /// the finished project. Progress + completion come from [`projectStatus`] polling.
  newProject(name: string, displayName: string, root?: string): Promise<ProjectStatus> {
    const params: CommandParamsMap["new-project"] = {
      name,
      displayName,
    };
    if (root !== undefined && root !== "") {
      params.root = root;
    }
    return call("new-project", params);
  },
  /// Kick off a non-blocking project open — returns the initial `Loading` status snapshot; follow
  /// progress via [`projectStatus`].
  openProject(path: string): Promise<ProjectStatus> {
    return call("open-project", { path });
  },
  appDataInfo(): Promise<AppDataInfo> {
    return invoke<AppDataInfo>("app_data_info");
  },
  listRecentProjects(): Promise<RecentProjects> {
    return invoke<RecentProjects>("list_recent_projects");
  },
  rememberRecentProject(project: RecentProject): Promise<RecentProjects> {
    return invoke<RecentProjects>("remember_recent_project", { project });
  },
  loadEditorSettings(): Promise<EditorSettings> {
    return invoke<EditorSettings>("load_editor_settings");
  },
  saveEditorSettings(settings: EditorSettings): Promise<void> {
    return invoke<void>("save_editor_settings", { settings });
  },

  renderStats(): Promise<RenderStats> {
    return call("render-stats");
  },

  /// Set the GPU profiler depth. `timestamps` enables per-pass GPU timing + the VMA
  /// budget read; `off` returns to baseline cost. Reports device capability.
  setProfilerMode(mode: ProfilerMode): Promise<ProfilerModeResult> {
    return call("profiler.set-mode", { mode });
  },
  /// Last frame's per-pass GPU times (needs the profiler in timestamps mode).
  passTimings(): Promise<RenderPassTimingsDto> {
    return call("pass-timings");
  },
  /// Arm a bounded profiler capture (single frame by default). Forces timestamps mode +
  /// sub-scopes for the duration; returns the capture id + ack.
  captureStart(params: CaptureStartParams): Promise<CaptureStartResult> {
    return call("profiler.capture-start", params);
  },
  /// Non-destructive capture progress — poll while recording to drive the live counter and
  /// detect readiness without draining the capture.
  captureStatus(): Promise<CaptureStatusResult> {
    return call("profiler.capture-status");
  },
  /// Finish + return the armed capture. A single capture comes back inline (`capture` +
  /// `chromeTrace`); a multi-frame one is written to `path`. `ready` is false when none armed.
  captureStop(): Promise<CaptureStopResult> {
    return call("profiler.capture-stop");
  },
  /// Frame-time percentiles + stutter count, optionally with the recent raw samples
  /// (the live-graph source). Always recorded, independent of the profiler.
  frameHistory(samples?: number): Promise<FrameHistoryDto> {
    return call("frame-history", samples === undefined ? {} : { samples });
  },
  /// The shared budget / green-amber-red threshold config.
  getPerfConfig(): Promise<PerfConfigDto> {
    return call("get-perf-config");
  },
  setPerfConfig(params: SetPerfConfigParams): Promise<PerfConfigDto> {
    return call("set-perf-config", params);
  },
  /// The TAAU ratio + dynamic-resolution state + live input/display extents.
  getUpscale(): Promise<GetUpscaleResult> {
    return call("get-upscale", {});
  },
  /// Partial update of the upscale surface (ratio / dynamic / targetMs); omitted fields hold.
  setUpscale(params: SetUpscaleParams): Promise<SetUpscaleResult> {
    return call("set-upscale", params);
  },
  /// Drain perf-alarm events with seq > since (non-blocking) plus the cursor metadata.
  drainAlarms(since: number): Promise<DrainAlarmsResult> {
    return call("drain-alarms", { since });
  },
  /// The currently firing perf alarms (the badge + per-pass highlight source).
  listActiveAlarms(): Promise<ActiveAlarmsDto> {
    return call("list-active-alarms");
  },
  getEnvironment(): Promise<Environment> {
    return call("get-environment");
  },
  setEnvironment(env: Partial<Environment>): Promise<Environment> {
    return call("set-environment", env);
  },
  /// Merge atmosphere fields over the current environment's `atmosphere` block; the
  /// engine re-bakes the LUT chain next frame. Returns the full updated environment.
  setAtmosphere(atmosphere: Partial<Environment["atmosphere"]>): Promise<Environment> {
    return call("set-atmosphere", atmosphere);
  },

  /// Anti-aliasing mode. Echoes `{ aa }`.
  setAa(mode: RenderStats["aa"]): Promise<{ aa: RenderStats["aa"] }> {
    return call("set-aa", { mode });
  },
  /// Debug render-output mode (read back via render-stats). Transient; not persisted. Echoes `{ viewMode }`.
  setViewMode(mode: RenderStats["viewMode"]): Promise<{ viewMode: RenderStats["viewMode"] }> {
    return call("set-view-mode", { mode });
  },
  /// Clustered (Forward+) light culling. Echoes `{ clustered }`.
  setClustered(on: boolean): Promise<{ clustered: boolean }> {
    return call("set-clustered", { enabled: on });
  },
  /// Image-based lighting. Echoes `{ ibl }`.
  setIbl(on: boolean): Promise<{ ibl: boolean }> {
    return call("set-ibl", { enabled: on });
  },
  /// The render-quality tier — the single knob for the SSGI / GTAO / contact-shadow stack.
  /// Echoes the resolved per-effect state.
  setRenderQuality(tier: string): Promise<RenderQualityResult> {
    return call("set-render-quality", { tier });
  },
  /// The active render-quality tier + resolved per-effect state.
  getRenderQuality(): Promise<RenderQualityResult> {
    return call("get-render-quality", {});
  },
  /// Reports the editor viewport visibility so the host idles a hidden view. `occluded` suppresses
  /// rendering entirely; `unfocused`/`focused` render on demand.
  setViewportPowerState(state: "focused" | "unfocused" | "occluded"): Promise<{ state: string }> {
    return call("set-viewport-power-state", { state });
  },
  /// The HDR→display tonemap operator. Echoes `{ mode }`.
  setTonemap(mode: "reinhard" | "aces" | "agx" | "pbr-neutral"): Promise<{ mode: string }> {
    return call("set-tonemap", { mode });
  },
  /// Shadow pass. Echoes `{ shadows }`.
  setShadows(on: boolean): Promise<{ shadows: boolean }> {
    return call("set-shadows", { enabled: on });
  },
  /// Global-illumination mode (`off` | `ddgi`). Echoes `{ ddgi }`.
  setGi(mode: "off" | "ddgi"): Promise<{ ddgi: boolean }> {
    return call("set-gi", { mode });
  },
  /// Depth pre-pass. Echoes `{ depthPrepass }`.
  setDepthPrepass(on: boolean): Promise<{ depthPrepass: boolean }> {
    return call("set-depth-prepass", { enabled: on });
  },
  /// Ray-traced shadows. Rejects with the typed error when ray tracing is
  /// unsupported on the device; echoes `{ rtShadows }` otherwise.
  setRtShadows(on: boolean): Promise<{ rtShadows: boolean }> {
    return call("set-rt-shadows", { enabled: on });
  },
  /// ReSTIR. Rejects with the typed error when ray tracing is unsupported;
  /// echoes `{ restir }` otherwise.
  setRestir(on: boolean): Promise<{ restir: boolean }> {
    return call("set-restir", { enabled: on });
  },
  /// Screen-space reflections (sharp mirror reflections on smooth surfaces).
  setSsr(on: boolean): Promise<{ ssr: boolean }> {
    return call("set-ssr", { enabled: on });
  },
  /// Ray-traced reflections (off-screen-aware). Rejects with the typed error
  /// when ray tracing is unsupported; echoes `{ rtReflections }` otherwise.
  setRtReflections(on: boolean): Promise<{ rtReflections: boolean }> {
    return call("set-rt-reflections", { enabled: on });
  },
  /// Tonemap exposure in stops (exp2). This is the EFFECTIVE exposure; the env's
  /// `exposure` field is reserved on the wire. Echoes `{ exposureEv }`.
  setExposure(ev: number): Promise<{ exposureEv: number }> {
    return call("set-exposure", { ev });
  },

  /// Write catalog + scene to `path` (engine default `project.json` when omitted).
  saveProject(path?: string): Promise<ProjectInfo> {
    return call("save-project", path === undefined ? {} : { path });
  },
  /// The project's enabled asset-store connector ids (the `stores` block).
  getStores(): Promise<CommandResultMap["get-stores"]> {
    return call("get-stores");
  },
  /// Set the project's enabled asset-store connector ids (persisted by save-project).
  setStores(enabled: string[]): Promise<CommandResultMap["set-stores"]> {
    return call("set-stores", { enabled });
  },
  /// Cook the loaded project into a standalone app folder at `outputDir`, writing `app` as the
  /// runtime manifest (`app.json`). Returns the staged path + any non-fatal cook warnings.
  exportApp(outputDir: string, app: AppManifest): Promise<ExportAppResult> {
    return call("export-app", { outputDir, app });
  },
  /// Kick off a non-blocking reload of the active project from its own path. Returns the initial
  /// `Loading` status snapshot; progress + completion come from [`projectStatus`] polling.
  reloadProject(): Promise<ProjectStatus> {
    return call("reload-project");
  },
  /// Write the scene only to `path` (required).
  saveScene(path: string): Promise<{ path: string }> {
    return call("save-scene", { path });
  },
  /// Load the scene only from `path` (required). Clears the engine's selection.
  loadScene(path: string): Promise<{ path: string }> {
    return call("load-scene", { path });
  },
  /// Capture a PNG. `viewport` is synchronous (`pending:false`); `window` is
  /// deferred and unavailable under embedded present-only mode.
  screenshot(
    target: "viewport" | "window",
    path: string,
  ): Promise<{ target: "viewport" | "window"; path: string; pending: boolean }> {
    return call("screenshot", { target, path });
  },

  viewportNativeInfo(): Promise<CommandResultMap["viewport-native-info"]> {
    return call("viewport-native-info");
  },

  /// Stream editor fly-cam input (pointer-lock look deltas in pixels + move keys).
  flyInput(input: {
    active: boolean;
    lookDx?: number;
    lookDy?: number;
    forward?: boolean;
    back?: boolean;
    left?: boolean;
    right?: boolean;
    up?: boolean;
    down?: boolean;
  }): Promise<{ active: boolean }> {
    return call("fly-input", input);
  },
  scriptInput(keys: string[]): Promise<{ keys: string[] }> {
    return call("script-input", { keys });
  },

  /// Engine lifecycle + presenter calls go through dedicated Rust commands, not the
  /// generic control passthrough.
  startEngine(): Promise<void> {
    return invoke<void>("start_engine");
  },
  /// Route one view's pane rect to its own subsurface (each view has its own surface, permanently glued
  /// to its pane — no shared subsurface re-bind on a tab switch). `resizeEngine` also commits that view's
  /// device-pixel render size to the engine — send it on settled bounds, not on live drag ticks.
  setViewportBounds(view: ViewId, bounds: ViewportBounds, resizeEngine: boolean): Promise<void> {
    return invoke<void>("set_viewport_bounds", { view, bounds, resizeEngine });
  },
  /// Park/unpark one view's subsurface (its tab isn't active, or a modal owns the region). A parked
  /// surface detaches; its ring retains the last frame, so unparking re-shows it instantly.
  setViewportParked(view: ViewId, parked: boolean): Promise<void> {
    return invoke<void>("set_viewport_parked", { view, parked });
  },
  /// The true monitor refresh (Hz) of the viewport's output, from the Wayland presenter's
  /// presentation feedback — the WebKitGTK webview's rAF is pinned to ~60 Hz and can't see it.
  /// `0` until the first presented frame reports it.
  viewportRefreshHz(): Promise<number> {
    return invoke<number>("viewport_refresh_hz");
  },
  quitEngine(): Promise<void> {
    return invoke<void>("quit_engine");
  },
  engineAlive(): Promise<boolean> {
    return invoke<boolean>("engine_alive");
  },
};

export type Client = typeof client;
export type { ProjectInfo, Vec3 };
