/// The CEF shell bridge: the editor's native APIs — invoke/listen/window/webview/dialog/Channel —
/// exposed to the React app. Everything crosses to native through CEF's message router
/// (`window.cefQuery`), which the shell dispatches by command name; events arrive by the shell
/// executing `window.__saffronShellEvent(name, payload)` in this frame. There is exactly one bridge
/// and one code path.

interface CefQuery {
  request: string;
  onSuccess: (response: string) => void;
  onFailure: (errorCode: number, errorMessage: string) => void;
  persistent?: boolean;
}

declare global {
  interface Window {
    cefQuery: (query: CefQuery) => number;
    cefQueryCancel: (id: number) => void;
    /// Installed once below; the shell calls it to deliver an event to `listen` subscribers.
    __saffronShellEvent?: (event: string, payload: unknown) => void;
  }
}

/// A rejected `invoke`, carrying the engine's machine-readable `code` when present. Shaped so
/// `control/client.ts`'s `toControlError` recovers both `message` and `code`, and `lib/flash.ts`'s
/// `errorText` (Error → `.message`) reads it directly.
export class InvokeError extends Error {
  readonly code?: string;
  constructor(message: string, code?: string) {
    super(message);
    this.name = "InvokeError";
    this.code = code;
  }
}

/// Call a native command by name with JSON args: resolves with the JSON result,
/// rejects with an [`InvokeError`]. Args are sent as `{ command, args }`; the reply is the handler's
/// JSON (or the `{ message, code }` failure the router passed to `onFailure`).
export function invoke<T = unknown>(command: string, args?: Record<string, unknown>): Promise<T> {
  return new Promise<T>((resolve, reject) => {
    if (typeof window.cefQuery !== "function") {
      reject(new InvokeError("cefQuery unavailable — not running inside the CEF shell"));
      return;
    }
    window.cefQuery({
      request: JSON.stringify({ command, args: args ?? {} }),
      onSuccess: (response) => {
        resolve((response ? JSON.parse(response) : null) as T);
      },
      onFailure: (_code, message) => {
        try {
          const parsed: unknown = JSON.parse(message);
          if (parsed && typeof parsed === "object" && "message" in parsed) {
            const obj = parsed as { message?: unknown; code?: unknown };
            reject(
              new InvokeError(
                typeof obj.message === "string" ? obj.message : message,
                typeof obj.code === "string" ? obj.code : undefined,
              ),
            );
            return;
          }
        } catch {
          // Not JSON — fall through to the raw string.
        }
        reject(new InvokeError(message));
      },
    });
  });
}

export type UnlistenFn = () => void;

type EventCallback<T> = (event: { payload: T }) => void;

const listeners = new Map<string, Set<EventCallback<unknown>>>();

if (typeof window !== "undefined") {
  window.__saffronShellEvent = (event: string, payload: unknown): void => {
    const set = listeners.get(event);
    if (!set) {
      return;
    }
    for (const callback of [...set]) {
      callback({ payload });
    }
  };
}

/// Subscribe to a shell event (`engine-phase`, `mouse-button`, `window-resized`, drag-drop, …).
/// Subscribe to a native event; the returned function unsubscribes.
export function listen<T>(
  event: string,
  callback: (event: { payload: T }) => void,
): Promise<UnlistenFn> {
  let set = listeners.get(event);
  if (!set) {
    set = new Set();
    listeners.set(event, set);
  }
  const typed = callback as EventCallback<unknown>;
  set.add(typed);
  return Promise.resolve(() => {
    set.delete(typed);
  });
}

let channelSeq = 0;

/// A progress channel, a progress channel. Serializes to `{ __saffronChannel: id }`
/// in an `invoke` arg; the native side streams messages back as `channel:{id}` events.
export class Channel<T = unknown> {
  readonly id: number;
  onmessage: ((message: T) => void) | null = null;

  constructor() {
    this.id = ++channelSeq;
    void listen<T>(`channel:${this.id}`, (event) => {
      this.onmessage?.(event.payload);
    });
  }

  toJSON(): { __saffronChannel: number } {
    return { __saffronChannel: this.id };
  }
}

/// The one editor toplevel — the window controls the editor uses. Controls
/// A window edge or corner an interactive resize is dragged from (the window-frame strips).
export type ResizeDirection =
  | "north"
  | "south"
  | "east"
  | "west"
  | "north-east"
  | "north-west"
  | "south-east"
  | "south-west";

/// marshal to the shell's main thread; reads are one round-trip; `onResized` fans out the shell's
/// `window-resized` event.
export function getCurrentWindow() {
  return {
    scaleFactor: (): Promise<number> => invoke<number>("window_scale_factor"),
    isMaximized: (): Promise<boolean> => invoke<boolean>("window_is_maximized"),
    minimize: (): Promise<void> => invoke<void>("window_minimize"),
    toggleMaximize: (): Promise<void> => invoke<void>("window_toggle_maximize"),
    close: (): Promise<void> => invoke<void>("window_close"),
    startResizeDragging: (direction: ResizeDirection): Promise<void> =>
      invoke<void>("window_start_resize", { direction }),
    /// Grab + hide the cursor for the RMB fly-cam (`true`), or release it (`false`). CEF's windowless
    /// OSR can't service DOM pointer lock, so the shell locks the cursor natively and streams relative
    /// motion back as `fly-look` events.
    setPointerLock: (locked: boolean): Promise<void> =>
      invoke<void>("set_pointer_lock", { locked }),
    show: (): Promise<void> => invoke<void>("window_show"),
    onResized: (callback: () => void): Promise<UnlistenFn> =>
      listen<unknown>("window-resized", () => callback()),
  };
}

/// A drag-drop event over the window — the payload union the frontend consumes.
export type DragDropPayload =
  | { type: "enter" | "over"; paths: string[]; position: { x: number; y: number } }
  | { type: "leave" }
  | { type: "drop"; paths: string[]; position: { x: number; y: number } };

/// The window's drag-drop surface. The shell emits `drag-drop` events from its Wayland
/// `wl_data_device` file-drop receiver, exposed as `getCurrentWebview().onDragDropEvent`.
export function getCurrentWebview() {
  return {
    onDragDropEvent: (
      callback: (event: { payload: DragDropPayload }) => void,
    ): Promise<UnlistenFn> => listen<DragDropPayload>("drag-drop", callback),
  };
}

/// A file-dialog filter (name + allowed extensions) for the native file dialog.
export interface DialogFilter {
  name: string;
  extensions: string[];
}

export interface OpenDialogOptions {
  multiple?: boolean;
  directory?: boolean;
  filters?: DialogFilter[];
  defaultPath?: string;
  title?: string;
}

export interface SaveDialogOptions {
  filters?: DialogFilter[];
  defaultPath?: string;
  title?: string;
}

/// Open a native file/folder picker. Returns the chosen path(s), or `null` if cancelled — a string
/// unless `multiple` is set (then a string array). Backed by the shell's native dialog command.
export function open(options?: OpenDialogOptions): Promise<string | string[] | null> {
  return invoke<string | string[] | null>("dialog_open", { options: options ?? {} });
}

/// Open a native save picker. Returns the chosen path, or `null` if cancelled.
export function save(options?: SaveDialogOptions): Promise<string | null> {
  return invoke<string | null>("dialog_save", { options: options ?? {} });
}
