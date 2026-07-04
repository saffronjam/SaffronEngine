// The Import split button: a primary "import the whole asset" action plus, when the asset
// exposes parts, a dropdown that imports a single map, and — when the asset has resolution
// variants — a resolution picker. Shared by the card and the detail modal.
import { useState } from "react";
import { ChevronDown, Download } from "lucide-react";

import { CircularProgress } from "./CircularProgress";
import { Button } from "@/components/ui/button";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { cn } from "@/lib/utils";

import type { StoreResult } from "./types";
import { useStoreImport } from "./useStoreImport";

// Offered import resolutions; connectors fall back to the nearest an asset actually provides.
const RESOLUTIONS = ["1K", "2K", "4K", "8K"];
const DEFAULT_RESOLUTION = "2K";

export function ImportControls({
  result,
  size = "sm",
  // Radix Select/DropdownMenu are deferred until the card is interactive — mounting one of each
  // per card would stall the virtualized grid's re-render on a fast scroll (blank rows). At rest a
  // static stand-in of the same size holds the layout; the real controls mount on hover, before
  // any interaction is possible. Defaults on, so the detail modal stays fully interactive.
  interactive = true,
}: {
  result: StoreResult;
  size?: "sm" | "default";
  interactive?: boolean;
}) {
  const { importing, progress, parts, partsLoading, importWhole, loadParts, importPart } =
    useStoreImport(result);
  const [resolution, setResolution] = useState(DEFAULT_RESOLUTION);
  const compact = size === "sm";
  const showResolution = result.supportsResolution;
  const iconSize = compact ? 14 : 16;

  return (
    <div className="flex items-center justify-end gap-1.5">
      {showResolution ? (
        interactive ? (
          <Select value={resolution} onValueChange={setResolution}>
            <SelectTrigger
              size="sm"
              className={cn(compact ? "!h-6 w-14 px-1.5 text-[11px]" : "h-8 w-20")}
              aria-label="Import resolution"
            >
              <SelectValue />
            </SelectTrigger>
            <SelectContent>
              {RESOLUTIONS.map((r) => (
                <SelectItem key={r} value={r}>
                  {r}
                </SelectItem>
              ))}
            </SelectContent>
          </Select>
        ) : (
          <div
            aria-hidden
            className={cn(
              "flex items-center justify-between gap-1 rounded-md border border-input bg-transparent text-muted-foreground",
              compact ? "h-6 w-14 px-1.5 text-[11px]" : "h-8 w-20 px-3 text-sm",
            )}
          >
            {resolution}
            <ChevronDown className="size-4 shrink-0 opacity-50" />
          </div>
        )
      ) : null}
      <div className="flex items-center justify-end gap-px">
        <Button
          type="button"
          size={compact ? "sm" : "default"}
          className={cn(compact && "h-6 text-[11px]", result.hasParts && "rounded-r-none")}
          disabled={importing}
          onClick={() => importWhole(showResolution ? resolution : undefined)}
        >
          {/* The download icon cross-fades into a live progress ring while importing. */}
          <span
            className="relative inline-flex shrink-0 items-center justify-center"
            style={{ width: iconSize, height: iconSize }}
          >
            <Download
              className={cn(
                "absolute transition-opacity duration-200",
                compact ? "size-3" : "size-4",
                importing && "opacity-0",
              )}
            />
            <span
              className={cn(
                "absolute transition-opacity duration-200",
                importing ? "opacity-100" : "opacity-0",
              )}
            >
              <CircularProgress value={progress ?? 0} size={iconSize} />
            </span>
          </span>
          Import
        </Button>
        {result.hasParts ? (
          interactive ? (
            <DropdownMenu onOpenChange={loadParts}>
              <DropdownMenuTrigger asChild>
                <Button
                  type="button"
                  size={compact ? "sm" : "default"}
                  className={cn("rounded-l-none p-0", compact ? "h-6 w-6" : "w-8")}
                  aria-label="Import individual files"
                >
                  <ChevronDown className={compact ? "size-3" : "size-4"} />
                </Button>
              </DropdownMenuTrigger>
              <DropdownMenuContent align="end" className="min-w-44">
                {partsLoading ? (
                  <div className="px-2 py-1.5 text-xs text-muted-foreground italic">Loading…</div>
                ) : parts && parts.length > 0 ? (
                  parts.map((part) => {
                    // The resolution selector governs per-map imports too; show that value.
                    const partRes = showResolution ? resolution : part.resolution;
                    return (
                      <DropdownMenuItem
                        key={part.id}
                        onSelect={() => importPart(part, showResolution ? resolution : undefined)}
                        className="text-xs"
                      >
                        {part.label}
                        {partRes ? (
                          <span className="ml-auto text-[10px] text-muted-foreground">
                            {partRes}
                          </span>
                        ) : null}
                      </DropdownMenuItem>
                    );
                  })
                ) : (
                  <div className="px-2 py-1.5 text-xs text-muted-foreground italic">
                    No individual files
                  </div>
                )}
              </DropdownMenuContent>
            </DropdownMenu>
          ) : (
            <Button
              type="button"
              size={compact ? "sm" : "default"}
              className={cn("rounded-l-none p-0", compact ? "h-6 w-6" : "w-8")}
              aria-label="Import individual files"
              tabIndex={-1}
            >
              <ChevronDown className={compact ? "size-3" : "size-4"} />
            </Button>
          )
        ) : null}
      </div>
    </div>
  );
}
