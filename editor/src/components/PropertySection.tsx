import { ChevronRight, RotateCcw } from "lucide-react";
import { Collapsible as CollapsiblePrimitive } from "radix-ui";
import { Button } from "@/components/ui/button";
import { cn } from "@/lib/utils";

interface PropertySectionProps {
  title: string;
  summary?: string;
  open: boolean;
  modified?: boolean;
  onOpenChange(open: boolean): void;
  onReset?(): void;
  children: React.ReactNode;
  className?: string;
}

/// A reusable progressive-disclosure group for dense property panels.
export function PropertySection({
  title,
  summary,
  open,
  modified = false,
  onOpenChange,
  onReset,
  children,
  className,
}: PropertySectionProps) {
  return (
    <CollapsiblePrimitive.Root
      open={open}
      onOpenChange={onOpenChange}
      className={cn("rounded-md border border-border/70 bg-card/30", className)}
    >
      <div className="flex min-h-8 items-center gap-1 px-1.5">
        <CollapsiblePrimitive.Trigger asChild>
          <button
            type="button"
            className="flex min-w-0 flex-1 items-center gap-1.5 rounded-sm px-1 py-1 text-left outline-none hover:bg-muted/60 focus-visible:ring-2 focus-visible:ring-ring/50"
          >
            <ChevronRight
              className={cn("size-3.5 shrink-0 transition-transform", open && "rotate-90")}
            />
            <span className="truncate text-[11px] font-medium">{title}</span>
            {modified ? (
              <span
                className="size-1.5 shrink-0 rounded-full bg-primary"
                aria-label="Contains non-default values"
              />
            ) : null}
            {!open && summary ? (
              <span className="ml-auto truncate text-[10px] text-muted-foreground">{summary}</span>
            ) : null}
          </button>
        </CollapsiblePrimitive.Trigger>
        {modified && onReset ? (
          <Button
            type="button"
            variant="ghost"
            size="icon-xs"
            aria-label={`Reset ${title}`}
            title={`Reset ${title}`}
            onClick={onReset}
          >
            <RotateCcw />
          </Button>
        ) : null}
      </div>
      <CollapsiblePrimitive.Content>
        <div className="flex flex-col gap-2 border-t border-border/60 p-2">{children}</div>
      </CollapsiblePrimitive.Content>
    </CollapsiblePrimitive.Root>
  );
}
