import { Label } from "@/components/ui/label";

/// A small uppercase caption naming a group of fields.
export function SectionLabel({ children }: { children: React.ReactNode }) {
  return (
    <Label className="text-[10px] font-medium uppercase tracking-wide text-muted-foreground">
      {children}
    </Label>
  );
}

/// A `SectionLabel` behind the rule that separates it from the group above.
export function SectionBreak({ children }: { children: React.ReactNode }) {
  return (
    <div className="mt-1 border-t border-border pt-2.5">
      <SectionLabel>{children}</SectionLabel>
    </div>
  );
}

/// A labelled field row: caption on the left, a fixed-width control column on the right.
export function FieldRow({ label, children }: { label: string; children: React.ReactNode }) {
  return (
    <div className="grid grid-cols-[1fr_120px] items-center gap-1.5">
      <Label className="truncate text-[11px] font-normal text-muted-foreground">{label}</Label>
      {children}
    </div>
  );
}

/// A labelled row whose control keeps its intrinsic width — switches, selects, button groups.
export function ControlRow({ label, children }: { label: string; children: React.ReactNode }) {
  return (
    <div className="grid grid-cols-[1fr_auto] items-center gap-1.5">
      <Label className="truncate text-[11px] font-normal text-muted-foreground">{label}</Label>
      {children}
    </div>
  );
}
