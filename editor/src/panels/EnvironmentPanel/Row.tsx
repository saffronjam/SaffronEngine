import { Label } from "@/components/ui/label";
import { Switch } from "@/components/ui/switch";
import { ColorField } from "../../components/ColorField";
import { NumberDrag } from "../../components/NumberDrag";
import { VectorEditor } from "../../components/VectorEditor";
import type { Vec3 } from "../../protocol";
import type { EnvironmentEditor } from "./useEnvironmentEditor";

/// A labelled row: a left caption plus the widget, matching the inspector's grid.
export function Row({ label, children }: { label: string; children: React.ReactNode }) {
  return (
    <div className="grid grid-cols-[96px_1fr] items-center gap-1.5">
      <Label className="truncate text-[11px] font-normal text-muted-foreground">{label}</Label>
      <div className="min-w-0">{children}</div>
    </div>
  );
}

/// A scrubbable scalar row, bracketed by the editor's drag gate so the reconcile poll stays off
/// for the whole gesture.
export function NumberRow({
  label,
  editor,
  value,
  min,
  max,
  step,
  onChange,
}: {
  label: string;
  editor: EnvironmentEditor;
  value: number;
  min?: number;
  max?: number;
  step?: number;
  onChange(value: number): void;
}) {
  return (
    <Row label={label}>
      <NumberDrag
        value={value}
        min={min}
        max={max}
        step={step}
        onChange={onChange}
        onDragStart={editor.onDragStart}
        onDragEnd={editor.onDragEnd}
      />
    </Row>
  );
}

export function SwitchRow({
  label,
  checked,
  onCheckedChange,
}: {
  label: string;
  checked: boolean;
  onCheckedChange(checked: boolean): void;
}) {
  return (
    <Row label={label}>
      <Switch checked={checked} onCheckedChange={onCheckedChange} />
    </Row>
  );
}

export function ColorRow({
  label,
  editor,
  value,
  onChange,
}: {
  label: string;
  editor: EnvironmentEditor;
  value: Vec3;
  onChange(channels: Record<string, number>): void;
}) {
  return (
    <Row label={label}>
      <ColorField
        kind="color3"
        value={value as unknown as Record<string, number>}
        onChange={onChange}
        onDragStart={editor.onDragStart}
        onDragEnd={editor.onDragEnd}
      />
    </Row>
  );
}

export function VectorRow({
  label,
  editor,
  axes,
  labels,
  value,
  step,
  onChange,
}: {
  label: string;
  editor: EnvironmentEditor;
  axes: string[];
  labels: string[];
  value: Record<string, number>;
  step: number;
  onChange(channels: Record<string, number>): void;
}) {
  return (
    <Row label={label}>
      <VectorEditor
        axes={axes}
        labels={labels}
        value={value}
        step={step}
        onChange={onChange}
        onDragStart={editor.onDragStart}
        onDragEnd={editor.onDragEnd}
      />
    </Row>
  );
}
