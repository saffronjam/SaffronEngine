/// Custom titlebar with editor view tabs. The tab strip and its drag mechanics are
/// the shared `TabStrip` (size "main"); this file keeps only the titlebar-local layers:
/// the window drag region + buttons and the per-kind tab icon.
import { getCurrentWindow } from "../shell";
import {
  Box,
  File,
  Flame,
  House,
  Image as ImageIcon,
  Maximize2,
  Minus,
  Square,
  Store,
  Workflow,
  X,
} from "lucide-react";
import { useEffect, useState } from "react";
import type { LucideIcon } from "lucide-react";
import type { ReactNode } from "react";
import { DRAG_REGION, IS_MACOS, NO_DRAG_REGION } from "../lib/platform";
import { useEditorStore, type ViewTab } from "../state/store";
import { TabStrip } from "@/components/dock/TabStrip";
import { Button } from "@/components/ui/button";

const appWindow = getCurrentWindow();

export function WindowTitlebar() {
  const [maximized, setMaximized] = useState(false);
  const tabs = useEditorStore((s) => s.viewTabs);
  const activeTabId = useEditorStore((s) => s.activeViewTabId);
  const setActiveViewTab = useEditorStore((s) => s.setActiveViewTab);
  const closeViewTab = useEditorStore((s) => s.closeViewTab);
  const moveViewTab = useEditorStore((s) => s.moveViewTab);
  const setHoveredTabId = useEditorStore((s) => s.setHoveredTabId);

  // Drop the hovered-tab target when the strip unmounts so a stale id never lingers.
  useEffect(() => () => setHoveredTabId(null), [setHoveredTabId]);

  useEffect(() => {
    let cancelled = false;

    const syncMaximized = async (): Promise<void> => {
      const nextMaximized = await appWindow.isMaximized();
      if (!cancelled) {
        setMaximized(nextMaximized);
      }
    };

    void syncMaximized();
    const unlisten = appWindow.onResized(() => {
      void syncMaximized();
    });

    return () => {
      cancelled = true;
      void unlisten.then((off) => off());
    };
  }, []);

  const minimize = (): void => {
    void appWindow.minimize();
  };

  const toggleMaximize = async (): Promise<void> => {
    await appWindow.toggleMaximize();
    setMaximized(await appWindow.isMaximized());
  };

  const close = (): void => {
    void appWindow.close();
  };

  const items = tabs.map((tab) => ({
    id: tab.id,
    title: tab.title,
    icon: tabIcon(tab),
    closable: tab.closable,
  }));

  return (
    // The titlebar's empty areas are a `-webkit-app-region: drag` surface: the shell reads these
    // rectangles from Chromium and starts a native window drag on a press (double-press maximizes).
    // Interactive children mark themselves `no-drag` so they stay clickable.
    <header
      className="flex h-9 flex-none items-center border-b border-border bg-card"
      style={DRAG_REGION}
    >
      {/* macOS draws native traffic lights at the top-left (transparent titlebar over a
          full-size content view); keep the tab strip clear of them. */}
      {IS_MACOS && <div className="w-20 flex-none self-stretch" />}
      <TabStrip
        items={items}
        activeId={activeTabId}
        size="main"
        className="h-full flex-none px-2"
        containerProps={{ style: NO_DRAG_REGION }}
        onActivate={(id) => setActiveViewTab(id)}
        onClose={closeViewTab}
        onTabHover={setHoveredTabId}
        drag={{ domain: "view", pinnedIds: ["scene"], onReorder: moveViewTab }}
      />
      <div className="min-w-0 flex-1 self-stretch" />
      {/* The window controls are drawn here only where the window has no native ones. */}
      {!IS_MACOS && (
        <div className="flex w-33 flex-none justify-end" style={NO_DRAG_REGION}>
          <TitlebarButton label="Minimize" onClick={minimize}>
            <Minus />
          </TitlebarButton>
          <TitlebarButton
            label={maximized ? "Restore" : "Maximize"}
            onClick={() => void toggleMaximize()}
          >
            {maximized ? <Square /> : <Maximize2 />}
          </TitlebarButton>
          <TitlebarButton label="Close" onClick={close} variant="close">
            <X />
          </TitlebarButton>
        </div>
      )}
    </header>
  );
}

function tabIcon(tab: ViewTab): LucideIcon {
  if (tab.kind === "scene") {
    return House;
  }
  if (tab.kind === "flamegraph") {
    return Flame;
  }
  if (tab.kind === "materialGraph") {
    return Workflow;
  }
  if (tab.kind === "assetEditor") {
    return Box;
  }
  if (tab.kind === "store") {
    return Store;
  }
  if (tab.assetType === "texture") {
    return ImageIcon;
  }
  return File;
}

type TitlebarButtonProps = {
  children: ReactNode;
  label: string;
  onClick: () => void;
  variant?: "default" | "close";
};

function TitlebarButton({ children, label, onClick, variant = "default" }: TitlebarButtonProps) {
  const className =
    variant === "close"
      ? "h-9 w-11 rounded-none hover:bg-destructive hover:text-destructive-foreground"
      : "h-9 w-11 rounded-none hover:bg-accent";

  return (
    <Button
      type="button"
      size="icon-sm"
      variant="ghost"
      className={className}
      aria-label={label}
      onClick={onClick}
    >
      {children}
    </Button>
  );
}
