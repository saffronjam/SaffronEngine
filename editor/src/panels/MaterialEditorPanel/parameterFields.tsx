import { useRef, useState } from "react";
import { ColorField } from "../../components/ColorField";
import { SliderField } from "../../components/SliderField";
import { notifyError } from "../../lib/flash";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";

/// The unit scale the material wire format stores a [0,1] parameter in, and the Q16.16 fixed-point
/// scale of its length parameters.
const UNIT_MAX = 65_535;
const FIXED_SCALE = 65_536;

export function ParameterGroup({ title, children }: { title: string; children: React.ReactNode }) {
  return (
    <div className="flex flex-col gap-2 border-t border-border pt-3">
      <h3 className="text-[10px] font-medium uppercase tracking-wide text-muted-foreground">
        {title}
      </h3>
      {children}
    </div>
  );
}

export function ParameterField({
  label,
  children,
  inline = false,
}: {
  label: string;
  children: React.ReactNode;
  inline?: boolean;
}) {
  return (
    <div className={inline ? "flex items-center justify-between gap-3" : "flex flex-col gap-1"}>
      <Label className="text-[11px] font-normal text-muted-foreground">{label}</Label>
      <div className={inline ? "flex-none" : "min-w-0"}>{children}</div>
    </div>
  );
}

export function UnitParameter({
  label,
  bits,
  minBits = 0,
  maxBits = UNIT_MAX,
  onChange,
  onDragStart,
  onDragEnd,
}: {
  label: string;
  bits: number;
  minBits?: number;
  maxBits?: number;
  onChange(bits: number): void;
  onDragStart(): void;
  onDragEnd(): void;
}) {
  return (
    <ParameterField label={label}>
      <SliderField
        value={bits / UNIT_MAX}
        min={minBits / UNIT_MAX}
        max={maxBits / UNIT_MAX}
        step={1 / UNIT_MAX}
        onChange={(value) => onChange(clampBits(Math.round(value * UNIT_MAX), minBits, maxBits))}
        onDragStart={onDragStart}
        onDragEnd={onDragEnd}
      />
    </ParameterField>
  );
}

export function ColorParameter({
  label,
  bits,
  maxBits = UNIT_MAX,
  onChange,
  onDragStart,
  onDragEnd,
}: {
  label: string;
  bits: [number, number, number];
  maxBits?: number;
  onChange(bits: [number, number, number]): void;
  onDragStart(): void;
  onDragEnd(): void;
}) {
  return (
    <ParameterField label={label}>
      <ColorField
        kind="color3"
        value={{ x: bits[0] / FIXED_SCALE, y: bits[1] / FIXED_SCALE, z: bits[2] / FIXED_SCALE }}
        onChange={(color) =>
          onChange([
            clampBits(Math.round((color.x ?? 0) * FIXED_SCALE), 0, maxBits),
            clampBits(Math.round((color.y ?? 0) * FIXED_SCALE), 0, maxBits),
            clampBits(Math.round((color.z ?? 0) * FIXED_SCALE), 0, maxBits),
          ])
        }
        onDragStart={onDragStart}
        onDragEnd={onDragEnd}
      />
    </ParameterField>
  );
}

export function CommitTextField({
  value,
  onCommit,
  validate,
  validationMessage,
  inputMode,
  placeholder,
}: {
  value: string;
  onCommit(value: string): void;
  validate(value: string): boolean;
  validationMessage: string;
  inputMode?: React.HTMLAttributes<HTMLInputElement>["inputMode"];
  placeholder?: string;
}) {
  const [draft, setDraft] = useState<string | null>(null);
  const cancel = useRef(false);
  const finish = (input: HTMLInputElement): void => {
    const next = input.value.trim();
    setDraft(null);
    if (cancel.current) {
      cancel.current = false;
      return;
    }
    if (!validate(next)) {
      notifyError(validationMessage);
      return;
    }
    if (next !== value) {
      onCommit(next);
    }
  };
  return (
    <Input
      value={draft ?? value}
      inputMode={inputMode}
      placeholder={placeholder}
      className="h-7 bg-background font-mono text-[11px]"
      onChange={(event) => setDraft(event.currentTarget.value)}
      onBlur={(event) => finish(event.currentTarget)}
      onKeyDown={(event) => {
        if (event.key === "Enter") {
          event.currentTarget.blur();
        } else if (event.key === "Escape") {
          cancel.current = true;
          event.currentTarget.blur();
        }
      }}
    />
  );
}

export function splitHashes(value: string): string[] {
  return value
    .split(/[\s,]+/)
    .map((hash) => hash.trim())
    .filter(Boolean);
}

export function clampBits(value: number, min: number, max: number): number {
  return Math.min(max, Math.max(min, value));
}
