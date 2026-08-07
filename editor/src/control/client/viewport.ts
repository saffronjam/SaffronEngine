import { invoke } from "../../shell";
import { call } from "./call";
import type {
  GizmoPointerPhase,
  SessionIntent,
  SessionStatus,
  ViewId,
  ViewportBounds,
} from "./types";
import type { CommandResultMap, EditorCamera, EntityRef, GizmoState, Vec3 } from "../../protocol";

/// The gizmo, the editor and preview cameras, input streaming, and the engine-lifecycle and
/// presenter calls — the latter go through dedicated Rust commands, not the control passthrough.
export const viewportCommands = {
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

  screenshot(
    target: "viewport" | "window",
    path: string,
  ): Promise<{ target: "viewport" | "window"; path: string; pending: boolean }> {
    return call("screenshot", { target, path });
  },

  viewportNativeInfo(): Promise<CommandResultMap["viewport-native-info"]> {
    return call("viewport-native-info");
  },

  scriptInput(keys: string[]): Promise<{ keys: string[] }> {
    return call("script-input", { keys });
  },

  /// Session lifecycle + presenter calls go through dedicated Rust commands, not the
  /// generic control passthrough. A session is one host process born for one project: the
  /// intent names the project to open or create (empty = the host resolves it from the
  /// environment), and stopping the session ends the process.
  sessionStart(intent: SessionIntent = {}): Promise<void> {
    return invoke<void>("session_start", { ...intent });
  },
  /// Route one view's pane rect to its own permanently-glued subsurface. `resizeEngine` also commits
  /// that view's device-pixel render size, so send it on settled bounds, not on live drag ticks.
  setViewportBounds(view: ViewId, bounds: ViewportBounds, resizeEngine: boolean): Promise<void> {
    return invoke<void>("set_viewport_bounds", { view, bounds, resizeEngine });
  },
  /// Park/unpark one view's subsurface (its tab isn't active, or a modal owns the region). A parked
  /// surface detaches; its ring retains the last frame, so unparking re-shows it instantly.
  setViewportParked(view: ViewId, parked: boolean): Promise<void> {
    return invoke<void>("set_viewport_parked", { view, parked });
  },
  /// The viewport output's true monitor refresh (Hz), from the presenter's presentation feedback,
  /// which the webview's rAF cannot see. `0` until the first presented frame reports it.
  viewportRefreshHz(): Promise<number> {
    return invoke<number>("viewport_refresh_hz");
  },
  sessionStop(): Promise<void> {
    return invoke<void>("session_stop");
  },
  sessionStatus(): Promise<SessionStatus> {
    return invoke<SessionStatus>("session_status");
  },
};
