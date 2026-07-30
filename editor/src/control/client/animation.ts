import { call } from "./call";
import type { ViewId } from "./types";
import type {
  AnimationStateResult,
  AssetModelResult,
  AssetPreviewOptionsResult,
  AssetPreviewResult,
  ClipBindingsResult,
  CommandResultMap,
  DebugOverlaysResult,
  ListClipsResult,
  MorphWeightsResult,
  PickSkeletonJointResult,
  PlayStateResult,
  SkeletonOverlayResult,
} from "../../protocol";

/// Clips and playheads, morph weights, the isolated asset-preview scene, and the overlays that
/// draw over either view.
export const animationCommands = {
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

  /// A model's capabilities, bone tree, and clips, read from its `.smodel` container. A model, mesh,
  /// or clip sub-asset all resolve to the same container; a static model returns an empty bone tree
  /// rather than an error.
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
    vegetationCells?: boolean;
    vegetationBounds?: boolean;
    vegetationRejections?: boolean;
    vegetationHeatmap?: boolean;
    vegetationNavigation?: boolean;
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
  setAssetPreviewOptions(opts: {
    floor?: boolean;
    variation?: number;
    phenotype?: number;
  }): Promise<AssetPreviewOptionsResult> {
    return call("set-asset-preview-options", opts);
  },
};
