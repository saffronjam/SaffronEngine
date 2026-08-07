/// The entity-add dropdown: every spawn preset plus a Named Empty flow. The engine auto-selects the
/// new entity, so the local selection mirrors it optimistically and the poll's sceneVersion bump
/// refreshes the hierarchy.
import { useState } from "react";
import type { LucideIcon } from "lucide-react";
import {
  Box,
  Camera,
  Circle,
  CircleDashed,
  CloudFog,
  Flashlight,
  Lightbulb,
  Orbit,
  Plus,
  Square,
  Sun,
} from "lucide-react";
import { client, type EntityPreset } from "../control/client";
import { recordEntityCreation, useEditorStore } from "../state/store";
import { errorText, notifyError } from "../lib/flash";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import { Popover, PopoverAnchor, PopoverContent } from "@/components/ui/popover";

/// The hand-maintained mapping of menu labels to `add-entity` presets and their
/// Lucide icons (the preset union itself comes from the typed client / protocol):
/// empty, the built-in primitives (cube, plane, sphere), point light, spot light,
/// directional light, camera, and reflection probe. Exported so other add surfaces can
/// reuse the same list.
export const CREATE_PRESETS: { label: string; preset: EntityPreset; icon: LucideIcon }[] = [
  { label: "Empty", preset: "empty", icon: CircleDashed },
  { label: "Cube", preset: "cube", icon: Box },
  { label: "Plane", preset: "plane", icon: Square },
  { label: "Sphere", preset: "sphere", icon: Circle },
  { label: "Point Light", preset: "point-light", icon: Lightbulb },
  { label: "Spot Light", preset: "spot-light", icon: Flashlight },
  { label: "Directional Light", preset: "directional-light", icon: Sun },
  { label: "Camera", preset: "camera", icon: Camera },
  { label: "Reflection Probe", preset: "reflection-probe", icon: Orbit },
  { label: "Fog Volume", preset: "fog-volume", icon: CloudFog },
];

export function CreateMenu() {
  const phase = useEditorStore((s) => s.engineStatus.phase);
  const selectEntity = useEditorStore((s) => s.selectEntity);
  const [namingOpen, setNamingOpen] = useState(false);

  const ready = phase === "ready";

  const create = (preset: EntityPreset): void => {
    void client
      .addEntity(preset)
      .then((ref) => {
        selectEntity(ref.id);
        recordEntityCreation(ref.id, "Create entity");
      })
      .catch((err: unknown) => notifyError(errorText(err)));
  };

  const createNamed = (name: string): void => {
    setNamingOpen(false);
    const trimmed = name.trim();
    if (trimmed === "") {
      return;
    }
    void client
      .createEntity(trimmed)
      .then((ref) => {
        selectEntity(ref.id);
        recordEntityCreation(ref.id, "Create entity");
      })
      .catch((err: unknown) => notifyError(errorText(err)));
  };

  return (
    <div className="flex min-w-0 items-center gap-2">
      <Popover open={namingOpen} onOpenChange={setNamingOpen}>
        <DropdownMenu>
          <PopoverAnchor asChild>
            <DropdownMenuTrigger asChild>
              <Button type="button" size="xs" variant="outline" disabled={!ready}>
                <Plus />
                <span>Add</span>
              </Button>
            </DropdownMenuTrigger>
          </PopoverAnchor>
          <DropdownMenuContent align="end" className="min-w-40">
            {CREATE_PRESETS.map(({ label, preset, icon: Icon }) => (
              <DropdownMenuItem key={preset} onSelect={() => create(preset)}>
                <Icon className="size-4 text-muted-foreground" />
                {label}
              </DropdownMenuItem>
            ))}
            <DropdownMenuSeparator />
            <DropdownMenuItem onSelect={() => setNamingOpen(true)}>
              <CircleDashed className="size-4 text-muted-foreground" />
              Named Empty…
            </DropdownMenuItem>
          </DropdownMenuContent>
        </DropdownMenu>
        <PopoverContent align="end" className="w-56 p-2">
          <NamedEmptyForm onCommit={createNamed} onCancel={() => setNamingOpen(false)} />
        </PopoverContent>
      </Popover>
    </div>
  );
}

/// Inline name entry for a named empty entity. Autofocuses; commits on Enter or the
/// Create button, cancels on Escape.
function NamedEmptyForm({
  onCommit,
  onCancel,
}: {
  onCommit(name: string): void;
  onCancel(): void;
}) {
  const [name, setName] = useState("");
  return (
    <form
      className="flex flex-col gap-2"
      onSubmit={(e) => {
        e.preventDefault();
        onCommit(name);
      }}
    >
      <Input
        autoFocus
        value={name}
        placeholder="Entity name"
        className="h-8 text-sm"
        onChange={(e) => setName(e.target.value)}
        onKeyDown={(e) => {
          if (e.key === "Escape") {
            e.preventDefault();
            onCancel();
          }
        }}
      />
      <Button type="submit" size="xs" disabled={name.trim() === ""}>
        Create
      </Button>
    </form>
  );
}
