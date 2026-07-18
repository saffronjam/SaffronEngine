/// The one tab strip used everywhere — the main view tabs and the compact dock strips.
/// Drag mechanics come entirely from `useTabStripDrag`, so the titlebar and every dock
/// leaf share one machine (parity by copy rots). Two size variants differ only in chrome:
/// `main` is the titlebar look (fixed-width rounded-top tabs); `dock` is the compact
/// shrink-to-fit look (`min-w-0` + truncate, so an overfull strip degrades by shrinking).
import { X } from "lucide-react";
import type { LucideIcon } from "lucide-react";
import { cva } from "class-variance-authority";
import type { HTMLAttributes, KeyboardEvent, MouseEvent } from "react";
import { cn } from "@/lib/utils";
import type { DockPanelId } from "../../state/dockLayout";
import { useTabStripDrag, type UseTabStripDragOptions } from "./useTabStripDrag";
import { useDockDrag } from "./dockDrag";
import { DockTabContextMenu } from "./DockTabContextMenu";

export interface TabStripItem {
  id: string;
  title: string;
  icon?: LucideIcon;
  closable: boolean;
}

export interface TabStripProps {
  items: TabStripItem[];
  activeId: string | null;
  size: "main" | "dock";
  onActivate(id: string): void;
  onClose(id: string): void;
  /// Called with the tab id the pointer enters and null when it leaves, so a mouse
  /// command (e.g. middle-click close) can target the hovered tab.
  onTabHover?(id: string | null): void;
  drag: Omit<UseTabStripDragOptions, "onActivate">;
  className?: string;
  /// Extra attributes for the strip container (e.g. a `-webkit-app-region: no-drag` style on the titlebar,
  /// `data-dock-strip` on a dock leaf's strip).
  containerProps?: HTMLAttributes<HTMLDivElement> & { [key: `data-${string}`]: string };
}

const STRIP_CONTAINER: Record<"main" | "dock", string> = {
  main: "flex min-w-0 items-end",
  dock: "flex h-8 min-w-0 flex-none items-center border-b border-border bg-background",
};

const tabVariants = cva(
  "flex select-none items-center font-medium transition-[transform,margin,background-color,border-color,color] duration-150 ease-out",
  {
    variants: {
      size: {
        main: "h-8 min-w-28 max-w-48 gap-2 rounded-t-md border px-3 text-left text-sm",
        dock: "-mb-px h-8 min-w-0 gap-1 border-b-2 pl-2.5 pr-1 text-xs",
      },
      active: { true: "", false: "" },
      draggable: { true: "cursor-default", false: "cursor-pointer" },
    },
    compoundVariants: [
      {
        size: "main",
        active: true,
        class: "border-border border-b-background bg-background text-foreground",
      },
      {
        size: "main",
        active: false,
        class:
          "border-transparent text-muted-foreground hover:bg-accent hover:text-accent-foreground",
      },
      { size: "dock", active: true, class: "border-primary text-foreground" },
      {
        size: "dock",
        active: false,
        class: "border-transparent text-muted-foreground hover:text-foreground",
      },
    ],
    defaultVariants: { size: "dock", active: false, draggable: false },
  },
);

export function TabStrip({
  items,
  activeId,
  size,
  onActivate,
  onClose,
  onTabHover,
  drag,
  className,
  containerProps,
}: TabStripProps) {
  const api = useTabStripDrag(
    items.map((item) => item.id),
    { ...drag, onActivate },
  );

  // A torn tab hovering THIS leaf parts the strip: tabs at/after the insertion index slide
  // by the ghost width to open a landing gap. The drag lives in the source strip's hook
  // instance, so this preview comes through the dockDrag channel — purely visual, no store
  // write, so every cancel path stays free.
  const dockDrag = useDockDrag();
  const gap =
    dockDrag &&
    drag.leafId !== undefined &&
    dockDrag.hovered?.kind === "tab" &&
    dockDrag.hovered.leafId === drag.leafId
      ? { index: dockDrag.hovered.index, width: dockDrag.ghostWidth, draggedId: dockDrag.panelId }
      : null;

  return (
    <div role="tablist" className={cn(STRIP_CONTAINER[size], className)} {...containerProps}>
      {items.map((item, index) => {
        const Icon = item.icon;
        const active = item.id === activeId;
        const dragged = api.draggedId === item.id && api.dragging;
        const gapStyle =
          gap && index >= gap.index && item.id !== gap.draggedId
            ? { transform: `translateX(${gap.width}px)` }
            : undefined;
        const button = (
          <button
            key={item.id}
            ref={(node) => api.registerTabRef(item.id, node)}
            type="button"
            role="tab"
            aria-selected={active}
            className={cn(
              tabVariants({ size, active, draggable: api.canDrag(item.id) }),
              dragged && "relative z-10 cursor-grabbing shadow-lg transition-none",
            )}
            style={{ ...api.styleFor(item.id), ...gapStyle }}
            onClick={() => {
              if (!api.canDrag(item.id)) {
                onActivate(item.id);
              }
            }}
            onPointerEnter={() => onTabHover?.(item.id)}
            onPointerLeave={() => onTabHover?.(null)}
            {...api.handlersFor(item.id)}
          >
            {Icon ? (
              <Icon className={cn("flex-none", size === "main" ? "size-4" : "size-3.5")} />
            ) : null}
            <span className="min-w-0 flex-1 truncate">{item.title}</span>
            {item.closable ? (
              <CloseAffordance size={size} title={item.title} onClose={() => onClose(item.id)} />
            ) : null}
          </button>
        );
        // Dock tabs get the right-click Move-to/Close menu (the no-drag fallback); the main
        // view tabs do not (they cannot dock).
        if (size === "dock" && drag.leafId !== undefined) {
          return (
            <DockTabContextMenu key={item.id} panelId={item.id as DockPanelId} leafId={drag.leafId}>
              {button}
            </DockTabContextMenu>
          );
        }
        return button;
      })}
    </div>
  );
}

function CloseAffordance({
  size,
  title,
  onClose,
}: {
  size: "main" | "dock";
  title: string;
  onClose(): void;
}) {
  const onClick = (event: MouseEvent): void => {
    event.stopPropagation();
    onClose();
  };
  const onKeyDown = (event: KeyboardEvent): void => {
    if (event.key === "Enter" || event.key === " ") {
      event.preventDefault();
      onClose();
    }
  };
  return (
    <span
      role="button"
      tabIndex={0}
      aria-label={`Close ${title}`}
      data-tab-close="true"
      className={cn(
        "flex flex-none items-center justify-center rounded-sm",
        size === "main"
          ? "size-5 hover:bg-accent"
          : "p-0.5 text-muted-foreground opacity-60 hover:bg-muted hover:opacity-100",
      )}
      onClick={onClick}
      onKeyDown={onKeyDown}
    >
      <X className={size === "main" ? "size-3.5" : "size-3"} />
    </span>
  );
}
