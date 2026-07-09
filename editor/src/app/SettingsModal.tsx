/// The Editor Settings modal (gear button in the titlebar): editor-wide preferences,
/// currently one section — Keyboard — listing every rebindable command from the
/// keybinding registry. Rows group by category and filter through the search box;
/// clicking a binding chip enters capture mode (the next keydown becomes the
/// binding, Esc cancels), an overridden row gets a reset button, and "Reset all"
/// clears every override after a confirm. Changes apply and persist immediately
/// (deltas to appdata/settings.json via the store) — there is no Apply/Cancel.
///
/// Same-scope conflicts are advisory, VS Code-style: the rebind is accepted and
/// every row whose effective binding collides inside its scope shows a warning.
import { useEffect, useMemo, useState } from "react";
import { listen, type UnlistenFn } from "../shell";
import { RotateCcw, TriangleAlert } from "lucide-react";
import {
  COMMANDS,
  bindingFor,
  findConflict,
  formatBinding,
  mouseToken,
  normalizePressEvent,
  type CommandDef,
  type CommandId,
} from "../lib/keybindings";
import { useEditorStore } from "../state/store";
import { ApiKeyField } from "../storefront/ApiKeyField";
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
import { ScrollArea } from "@/components/ui/scroll-area";
import { Tooltip, TooltipContent, TooltipTrigger } from "@/components/ui/tooltip";
import { cn } from "@/lib/utils";

/// Sections of the settings modal.
const SECTIONS = [
  { id: "keyboard", label: "Keyboard" },
  { id: "layout", label: "Layout" },
  { id: "api", label: "API & Secrets" },
] as const;

type SectionId = (typeof SECTIONS)[number]["id"];

export function SettingsModal() {
  const open = useEditorStore((s) => s.settingsOpen);
  const setSettingsOpen = useEditorStore((s) => s.setSettingsOpen);
  const [section, setSection] = useState<SectionId>("keyboard");
  const [capturingId, setCapturingId] = useState<CommandId | null>(null);

  // Closing the modal always leaves capture mode behind.
  useEffect(() => {
    if (!open) {
      setCapturingId(null);
    }
  }, [open]);

  return (
    <Dialog open={open} onOpenChange={setSettingsOpen}>
      <DialogContent
        className="flex h-[480px] flex-col sm:max-w-2xl"
        aria-describedby={undefined}
        // While capturing, Escape cancels the capture (handled by the capture
        // listener) and must not also close the dialog.
        onEscapeKeyDown={(event) => {
          if (capturingId) {
            event.preventDefault();
          }
        }}
      >
        <DialogHeader>
          <DialogTitle>Settings</DialogTitle>
        </DialogHeader>
        <div className="flex min-h-0 flex-1 gap-4">
          <nav className="w-32 flex-none border-r border-border pr-2">
            {SECTIONS.map((sec) => (
              <button
                key={sec.id}
                type="button"
                onClick={() => {
                  setSection(sec.id);
                  setCapturingId(null);
                }}
                className={cn(
                  "w-full rounded-md px-2.5 py-1.5 text-left text-sm font-medium",
                  section === sec.id
                    ? "bg-accent text-foreground"
                    : "text-muted-foreground hover:bg-accent/40 hover:text-foreground",
                )}
              >
                {sec.label}
              </button>
            ))}
          </nav>
          {section === "keyboard" ? (
            <KeyboardSection capturingId={capturingId} setCapturingId={setCapturingId} />
          ) : section === "layout" ? (
            <LayoutSection />
          ) : (
            <ApiSecretsSection />
          )}
        </div>
      </DialogContent>
    </Dialog>
  );
}

/// The API & Secrets section: paste/clear API keys for store connectors that need one.
/// Keys are stored in the OS keyring on this machine only — never in the project file.
function ApiSecretsSection() {
  return (
    <div className="flex min-h-0 min-w-0 flex-1 flex-col gap-3">
      <div className="rounded-md border border-border p-3">
        <p className="text-sm font-medium text-foreground">Poly Pizza</p>
        <p className="mb-2 text-xs text-muted-foreground">
          A free key (poly.pizza account) enables Poly Pizza in the asset Store. Stored only on this
          machine.
        </p>
        <ApiKeyField connectorId="poly-pizza" displayName="Poly Pizza" />
      </div>
    </div>
  );
}

/// The Layout section: reset every dock layout (Scene + Asset Editor) to its default
/// positions. Open panels stay open — only their positions reset.
function LayoutSection() {
  const resetDockLayout = useEditorStore((s) => s.resetDockLayout);
  return (
    <div className="flex min-h-0 min-w-0 flex-1 flex-col gap-3">
      <div className="flex items-start justify-between gap-4 rounded-md border border-border p-3">
        <div className="min-w-0">
          <p className="text-sm font-medium text-foreground">Reset layout</p>
          <p className="text-xs text-muted-foreground">
            Snap every panel back to its default position. Open panels stay open.
          </p>
        </div>
        <Button
          type="button"
          size="sm"
          variant="outline"
          className="flex-none"
          onClick={() => resetDockLayout()}
        >
          Reset layout
        </Button>
      </div>
    </div>
  );
}

function KeyboardSection({
  capturingId,
  setCapturingId,
}: {
  capturingId: CommandId | null;
  setCapturingId(id: CommandId | null): void;
}) {
  const keyBindings = useEditorStore((s) => s.keyBindings);
  const setKeyBinding = useEditorStore((s) => s.setKeyBinding);
  const resetKeyBinding = useEditorStore((s) => s.resetKeyBinding);
  const resetAllKeyBindings = useEditorStore((s) => s.resetAllKeyBindings);
  const [search, setSearch] = useState("");
  const [confirmResetAll, setConfirmResetAll] = useState(false);

  const capturing = capturingId ? COMMANDS.find((def) => def.id === capturingId) : undefined;

  // The capture listener: capture-phase on window so it pre-empts the global
  // shortcut hook and Radix's own Escape handling while listening. Mouse commands
  // capture a button (middle via the DOM, the side buttons via the native bridge
  // event, since WebKitGTK never hands those to the page); Esc cancels either kind.
  useEffect(() => {
    if (!capturing) {
      return;
    }
    if (capturing.kind === "mouse") {
      const onKeyDown = (event: KeyboardEvent): void => {
        if (event.key === "Escape") {
          event.preventDefault();
          event.stopPropagation();
          setCapturingId(null);
        }
      };
      const onPointerDown = (event: PointerEvent): void => {
        if (event.button !== 1) {
          return; // Only the middle button is capturable from the DOM.
        }
        event.preventDefault();
        event.stopPropagation();
        setKeyBinding(capturing.id, mouseToken("middle"));
        setCapturingId(null);
      };
      let off: UnlistenFn | null = null;
      let disposed = false;
      void listen<number>("mouse-button", (event) => {
        const name = event.payload === 8 ? "back" : event.payload === 9 ? "forward" : null;
        if (name) {
          setKeyBinding(capturing.id, mouseToken(name));
          setCapturingId(null);
        }
      }).then((fn) => (disposed ? fn() : (off = fn)));
      window.addEventListener("keydown", onKeyDown, { capture: true });
      window.addEventListener("pointerdown", onPointerDown, { capture: true });
      return () => {
        disposed = true;
        off?.();
        window.removeEventListener("keydown", onKeyDown, { capture: true });
        window.removeEventListener("pointerdown", onPointerDown, { capture: true });
      };
    }
    const onKeyDown = (event: KeyboardEvent): void => {
      event.preventDefault();
      event.stopPropagation();
      if (event.key === "Escape") {
        setCapturingId(null);
        return;
      }
      if (capturing.kind === "hold") {
        setKeyBinding(capturing.id, event.code);
        setCapturingId(null);
        return;
      }
      const value = normalizePressEvent(event);
      if (value === null) {
        return; // A bare modifier; keep listening.
      }
      setKeyBinding(capturing.id, value);
      setCapturingId(null);
    };
    window.addEventListener("keydown", onKeyDown, { capture: true });
    return () => window.removeEventListener("keydown", onKeyDown, { capture: true });
  }, [capturing, setCapturingId, setKeyBinding]);

  // Effective-binding conflicts (same scope only), recomputed per change.
  const conflicts = useMemo(() => {
    const map = new Map<CommandId, CommandId>();
    for (const def of COMMANDS) {
      const other = findConflict(def.id, bindingFor(def.id, keyBindings), keyBindings);
      if (other) {
        map.set(def.id, other);
      }
    }
    return map;
  }, [keyBindings]);

  const query = search.trim().toLowerCase();
  const visible = COMMANDS.filter(
    (def) =>
      query === "" ||
      def.label.toLowerCase().includes(query) ||
      def.category.toLowerCase().includes(query),
  );
  const categories = [...new Set(visible.map((def) => def.category))];

  return (
    <div className="flex min-h-0 min-w-0 flex-1 flex-col gap-2">
      <Input
        value={search}
        placeholder="Search commands…"
        className="h-8"
        onChange={(event) => setSearch(event.currentTarget.value)}
      />
      <ScrollArea className="min-h-0 flex-1 rounded-md border border-border">
        <div className="p-2">
          {visible.length === 0 ? (
            <p className="px-1 py-3 text-center text-xs italic text-muted-foreground">
              No commands match
            </p>
          ) : (
            categories.map((category) => (
              <div key={category} className="mb-2 last:mb-0">
                <p className="px-1 py-1 text-xs font-semibold uppercase tracking-wide text-muted-foreground">
                  {category}
                </p>
                {visible
                  .filter((def) => def.category === category)
                  .map((def) => (
                    <BindingRow
                      key={def.id}
                      def={def}
                      overrides={keyBindings}
                      capturing={capturingId === def.id}
                      conflictWith={conflicts.get(def.id) ?? null}
                      onCapture={() => setCapturingId(def.id)}
                      onReset={() => resetKeyBinding(def.id)}
                    />
                  ))}
              </div>
            ))
          )}
        </div>
      </ScrollArea>
      <div className="flex flex-none justify-end">
        <Button
          type="button"
          size="sm"
          variant="outline"
          disabled={Object.keys(keyBindings).length === 0}
          onClick={() => setConfirmResetAll(true)}
        >
          Reset all
        </Button>
      </div>
      <Dialog open={confirmResetAll} onOpenChange={setConfirmResetAll}>
        <DialogContent showCloseButton={false} className="sm:max-w-sm">
          <DialogHeader>
            <DialogTitle>Reset all key bindings?</DialogTitle>
            <DialogDescription>Every command returns to its default key.</DialogDescription>
          </DialogHeader>
          <DialogFooter>
            <Button type="button" variant="outline" onClick={() => setConfirmResetAll(false)}>
              Cancel
            </Button>
            <Button
              type="button"
              variant="destructive"
              onClick={() => {
                setConfirmResetAll(false);
                resetAllKeyBindings();
              }}
            >
              Reset all
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </div>
  );
}

function BindingRow({
  def,
  overrides,
  capturing,
  conflictWith,
  onCapture,
  onReset,
}: {
  def: CommandDef;
  overrides: Record<string, string>;
  capturing: boolean;
  conflictWith: CommandId | null;
  onCapture(): void;
  onReset(): void;
}) {
  const overridden = def.id in overrides;
  const conflictLabel = conflictWith
    ? COMMANDS.find((other) => other.id === conflictWith)?.label
    : null;

  return (
    <div
      className={cn(
        "flex items-center gap-2 rounded-md px-1 py-1 hover:bg-accent/40",
        conflictWith && "bg-destructive/10",
      )}
    >
      <span className="min-w-0 flex-1 truncate text-sm text-foreground">{def.label}</span>
      {conflictLabel ? (
        <span className="flex flex-none items-center gap-1 text-xs text-destructive">
          <TriangleAlert className="size-3.5" />
          Also bound to {conflictLabel}
        </span>
      ) : null}
      {overridden && !capturing ? (
        <Tooltip>
          <TooltipTrigger asChild>
            <Button type="button" size="icon-xs" variant="ghost" onClick={onReset}>
              <RotateCcw />
            </Button>
          </TooltipTrigger>
          <TooltipContent>Reset to default ({formatBinding(def, def.default)})</TooltipContent>
        </Tooltip>
      ) : null}
      <Button
        type="button"
        size="sm"
        variant="outline"
        className={cn("w-44 flex-none font-mono text-xs", capturing && "ring-1 ring-ring")}
        onClick={onCapture}
      >
        {capturing
          ? def.kind === "mouse"
            ? "Press a mouse button…"
            : "Press any key…"
          : formatBinding(def, bindingFor(def.id, overrides))}
      </Button>
    </div>
  );
}
