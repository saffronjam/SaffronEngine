/// The keybinding registry: every rebindable editor command, its default key, and the
/// parse/match/format helpers the handlers and the settings modal share. Overrides live in the store
/// as deltas only; handlers call `matchesBinding(event, id, overrides)` rather than comparing key
/// literals.
///
/// Three command kinds: "press" one-shots matched on a normalized key-string with exact modifiers,
/// so a binding of "f" does not fire on Ctrl+F and OS chords pass through untouched; "hold"
/// fly-camera keys matched on the physical `event.code`; and "mouse" buttons bound to a
/// `mouse:<name>` token, rebindable only to another mouse button so their value-space never overlaps
/// the keys.
export type CommandKind = "press" | "hold" | "mouse";

/// Conflict scope: bindings only collide within one scope. Global press commands
/// share one window listener; fly keys share the viewport fly listener; the
/// hierarchy/assets deletes are focus-scoped to their own panels, so the same key
/// in both is fine; tab mouse commands share the mouse dispatcher.
export type CommandScope = "global" | "hierarchy" | "assets" | "fly" | "tabs" | "vegetation";

export type CommandId =
  | "gizmo.translate"
  | "gizmo.rotate"
  | "gizmo.scale"
  | "camera.focus"
  | "selection.deselect"
  | "edit.undo"
  | "edit.redo"
  | "tab.navBack"
  | "tab.navForward"
  | "tab.close"
  | "hierarchy.delete"
  | "assets.delete"
  | "camera.flyForward"
  | "camera.flyBack"
  | "camera.flyLeft"
  | "camera.flyRight"
  | "camera.flyUp"
  | "camera.flyDown"
  | "vegetation.tool.select"
  | "vegetation.tool.lasso"
  | "vegetation.tool.paint"
  | "vegetation.tool.erase"
  | "vegetation.tool.density"
  | "vegetation.tool.reapply"
  | "vegetation.tool.single"
  | "vegetation.tool.fill"
  | "vegetation.tool.spline"
  | "vegetation.tool.volume"
  | "vegetation.tool.exclude"
  | "vegetation.tool.pin"
  | "vegetation.tool.promote"
  | "vegetation.brushGrow"
  | "vegetation.brushShrink"
  | "vegetation.delete";

export interface CommandDef {
  id: CommandId;
  label: string;
  category: string;
  kind: CommandKind;
  /// Key-string for "press" commands, `event.code` for "hold" commands.
  default: string;
  scope: CommandScope;
}

/// Registry, in display order for the settings modal.
export const COMMANDS: readonly CommandDef[] = [
  {
    id: "gizmo.translate",
    label: "Translate gizmo",
    category: "Gizmo",
    kind: "press",
    default: "w",
    scope: "global",
  },
  {
    id: "gizmo.rotate",
    label: "Rotate gizmo",
    category: "Gizmo",
    kind: "press",
    default: "e",
    scope: "global",
  },
  {
    id: "gizmo.scale",
    label: "Scale gizmo",
    category: "Gizmo",
    kind: "press",
    default: "r",
    scope: "global",
  },
  {
    id: "camera.focus",
    label: "Focus selection",
    category: "Camera",
    kind: "press",
    default: "f",
    scope: "global",
  },
  {
    id: "selection.deselect",
    label: "Deselect",
    category: "Selection",
    kind: "press",
    default: "escape",
    scope: "global",
  },
  {
    id: "edit.undo",
    label: "Undo",
    category: "Edit",
    kind: "press",
    default: "ctrl+z",
    scope: "global",
  },
  {
    id: "edit.redo",
    label: "Redo",
    category: "Edit",
    kind: "press",
    default: "ctrl+shift+z",
    scope: "global",
  },
  {
    id: "tab.navBack",
    label: "Navigate back",
    category: "Tabs",
    kind: "mouse",
    default: "mouse:back",
    scope: "tabs",
  },
  {
    id: "tab.navForward",
    label: "Navigate forward",
    category: "Tabs",
    kind: "mouse",
    default: "mouse:forward",
    scope: "tabs",
  },
  {
    id: "tab.close",
    label: "Close hovered tab",
    category: "Tabs",
    kind: "mouse",
    default: "mouse:middle",
    scope: "tabs",
  },
  {
    id: "hierarchy.delete",
    label: "Delete entity",
    category: "Hierarchy",
    kind: "press",
    default: "delete",
    scope: "hierarchy",
  },
  {
    id: "assets.delete",
    label: "Delete asset / folder",
    category: "Assets",
    kind: "press",
    default: "delete",
    scope: "assets",
  },
  {
    id: "camera.flyForward",
    label: "Fly forward",
    category: "Fly camera",
    kind: "hold",
    default: "KeyW",
    scope: "fly",
  },
  {
    id: "camera.flyBack",
    label: "Fly back",
    category: "Fly camera",
    kind: "hold",
    default: "KeyS",
    scope: "fly",
  },
  {
    id: "camera.flyLeft",
    label: "Fly left",
    category: "Fly camera",
    kind: "hold",
    default: "KeyA",
    scope: "fly",
  },
  {
    id: "camera.flyRight",
    label: "Fly right",
    category: "Fly camera",
    kind: "hold",
    default: "KeyD",
    scope: "fly",
  },
  {
    id: "camera.flyUp",
    label: "Fly up",
    category: "Fly camera",
    kind: "hold",
    default: "Space",
    scope: "fly",
  },
  {
    id: "camera.flyDown",
    label: "Fly down",
    category: "Fly camera",
    kind: "hold",
    default: "ShiftLeft",
    scope: "fly",
  },
  {
    id: "vegetation.tool.select",
    label: "Select tool",
    category: "Vegetation",
    kind: "press",
    default: "1",
    scope: "vegetation",
  },
  {
    id: "vegetation.tool.lasso",
    label: "Lasso tool",
    category: "Vegetation",
    kind: "press",
    default: "2",
    scope: "vegetation",
  },
  {
    id: "vegetation.tool.paint",
    label: "Paint tool",
    category: "Vegetation",
    kind: "press",
    default: "3",
    scope: "vegetation",
  },
  {
    id: "vegetation.tool.erase",
    label: "Erase tool",
    category: "Vegetation",
    kind: "press",
    default: "4",
    scope: "vegetation",
  },
  {
    id: "vegetation.tool.density",
    label: "Density tool",
    category: "Vegetation",
    kind: "press",
    default: "5",
    scope: "vegetation",
  },
  {
    id: "vegetation.tool.reapply",
    label: "Reapply tool",
    category: "Vegetation",
    kind: "press",
    default: "6",
    scope: "vegetation",
  },
  {
    id: "vegetation.tool.single",
    label: "Single tool",
    category: "Vegetation",
    kind: "press",
    default: "7",
    scope: "vegetation",
  },
  {
    id: "vegetation.tool.fill",
    label: "Fill tool",
    category: "Vegetation",
    kind: "press",
    default: "8",
    scope: "vegetation",
  },
  {
    id: "vegetation.tool.spline",
    label: "Spline tool",
    category: "Vegetation",
    kind: "press",
    default: "9",
    scope: "vegetation",
  },
  {
    id: "vegetation.tool.volume",
    label: "Volume tool",
    category: "Vegetation",
    kind: "press",
    default: "0",
    scope: "vegetation",
  },
  {
    id: "vegetation.tool.exclude",
    label: "Exclude tool",
    category: "Vegetation",
    kind: "press",
    default: "shift+1",
    scope: "vegetation",
  },
  {
    id: "vegetation.tool.pin",
    label: "Pin tool",
    category: "Vegetation",
    kind: "press",
    default: "shift+2",
    scope: "vegetation",
  },
  {
    id: "vegetation.tool.promote",
    label: "Promote tool",
    category: "Vegetation",
    kind: "press",
    default: "shift+3",
    scope: "vegetation",
  },
  {
    id: "vegetation.brushGrow",
    label: "Grow brush",
    category: "Vegetation",
    kind: "press",
    default: "]",
    scope: "vegetation",
  },
  {
    id: "vegetation.brushShrink",
    label: "Shrink brush",
    category: "Vegetation",
    kind: "press",
    default: "[",
    scope: "vegetation",
  },
  {
    id: "vegetation.delete",
    label: "Delete selected plant",
    category: "Vegetation",
    kind: "press",
    default: "delete",
    scope: "vegetation",
  },
];

export const COMMANDS_BY_ID: Record<CommandId, CommandDef> = Object.fromEntries(
  COMMANDS.map((def) => [def.id, def]),
) as Record<CommandId, CommandDef>;

/// True when `value` names a registered command (filters stale settings.json keys).
export function isCommandId(value: string): value is CommandId {
  return value in COMMANDS_BY_ID;
}

/// The mouse buttons that can be bound, by token. `back`/`forward` are the side buttons
/// (GDK 8/9, intercepted natively since WebKitGTK eats them); `middle` is the wheel click.
export type MouseButtonName = "middle" | "back" | "forward";

const MOUSE_LABELS: Record<string, string> = {
  "mouse:middle": "Middle button",
  "mouse:back": "Back button",
  "mouse:forward": "Forward button",
};

/// The binding token for a mouse button.
export function mouseToken(name: MouseButtonName): string {
  return `mouse:${name}`;
}

/// The mouse command (if any) whose effective binding equals `token`, in registry order.
export function mouseCommandFor(
  token: string,
  overrides: Record<string, string>,
): CommandId | null {
  for (const def of COMMANDS) {
    if (def.kind === "mouse" && bindingFor(def.id, overrides) === token) {
      return def.id;
    }
  }
  return null;
}

interface KeyEventLike {
  key: string;
  code: string;
  ctrlKey: boolean;
  shiftKey: boolean;
  altKey: boolean;
  metaKey: boolean;
}

const MODIFIER_KEYS = new Set(["Control", "Shift", "Alt", "Meta"]);

/// Normalize a keydown into a press key-string ("shift+f"), or null when the event
/// carries no main key (a pure-modifier press, e.g. Shift alone).
export function normalizePressEvent(event: KeyEventLike): string | null {
  if (MODIFIER_KEYS.has(event.key)) {
    return null;
  }
  let key = event.key.toLowerCase();
  if (key === " ") {
    key = "space";
  }
  let prefix = "";
  if (event.ctrlKey) {
    prefix += "ctrl+";
  }
  if (event.shiftKey) {
    prefix += "shift+";
  }
  if (event.altKey) {
    prefix += "alt+";
  }
  if (event.metaKey) {
    prefix += "meta+";
  }
  return prefix + key;
}

/// The effective binding for a command: the user override, else the default.
export function bindingFor(id: CommandId, overrides: Record<string, string>): string {
  return overrides[id] ?? COMMANDS_BY_ID[id].default;
}

/// True when the keydown matches the command's effective binding. Press commands
/// compare the normalized key-string (exact modifier set); hold commands compare
/// the physical `event.code`.
export function matchesBinding(
  event: KeyEventLike,
  id: CommandId,
  overrides: Record<string, string>,
): boolean {
  const binding = bindingFor(id, overrides);
  if (COMMANDS_BY_ID[id].kind === "hold") {
    return event.code === binding;
  }
  return normalizePressEvent(event) === binding;
}

const PRESS_KEY_LABELS: Record<string, string> = {
  escape: "Esc",
  delete: "Delete",
  backspace: "Backspace",
  space: "Space",
  enter: "Enter",
  tab: "Tab",
  arrowup: "Up",
  arrowdown: "Down",
  arrowleft: "Left",
  arrowright: "Right",
};

const MODIFIER_LABELS: Record<string, string> = {
  ctrl: "Ctrl",
  shift: "Shift",
  alt: "Alt",
  meta: "Meta",
};

/// Display label for a physical `event.code`: "KeyW" → "W", "Digit3" → "3",
/// "ShiftLeft" → "Left Shift", anything else verbatim.
function formatCode(code: string): string {
  if (code.startsWith("Key") && code.length === 4) {
    return code.slice(3);
  }
  if (code.startsWith("Digit") && code.length === 6) {
    return code.slice(5);
  }
  const side = code.match(/^(Shift|Control|Alt|Meta)(Left|Right)$/);
  if (side) {
    return `${side[2]} ${side[1] === "Control" ? "Ctrl" : side[1]}`;
  }
  return code;
}

/// Human-readable form of a binding value for chips and tooltips:
/// "shift+f" → "Shift+F", "escape" → "Esc", "KeyW" (hold) → "W",
/// "mouse:back" → "Back button".
export function formatBinding(def: CommandDef, value: string): string {
  if (def.kind === "mouse") {
    return MOUSE_LABELS[value] ?? value;
  }
  if (def.kind === "hold") {
    return formatCode(value);
  }
  const parts = value.split("+");
  const key = parts[parts.length - 1];
  const mods = parts.slice(0, -1).map((mod) => MODIFIER_LABELS[mod] ?? mod);
  const keyLabel = PRESS_KEY_LABELS[key] ?? (key.length === 1 ? key.toUpperCase() : key);
  return [...mods, keyLabel].join("+");
}

/// The command (if any) whose effective binding already equals `candidate` within
/// the same conflict scope as `forId`, excluding `forId` itself.
export function findConflict(
  forId: CommandId,
  candidate: string,
  overrides: Record<string, string>,
): CommandId | null {
  const scope = COMMANDS_BY_ID[forId].scope;
  for (const def of COMMANDS) {
    if (def.id === forId || def.scope !== scope) {
      continue;
    }
    if (bindingFor(def.id, overrides) === candidate) {
      return def.id;
    }
  }
  return null;
}
