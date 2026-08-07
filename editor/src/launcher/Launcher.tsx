/// The launcher view: an opaque, full-viewport surface shown whenever no project is loaded, a
/// load is in flight, a load failed, or the session crashed — the dot-grid backdrop with one
/// centered card (picker / boot progress / load failure / crash). A first-class view, not a
/// dialog: nothing renders behind it, so there is no modal machinery, no scroll lock, and no
/// exit-animation coordination with the viewport beyond the single fade below.
///
/// Mounted once in `App.tsx`. Visibility is derived; `launcherOpen` only forces the picker over a
/// live project (menu "New Project…", a load-failure "Back").
import { useEffect, useRef, useState } from "react";
import { X } from "lucide-react";
import { useEditorStore } from "../state/store";
import { cn } from "@/lib/utils";
import { Button } from "@/components/ui/button";
import { DotGrid } from "./DotGrid";
import { LauncherTitlebar } from "./LauncherTitlebar";
import { PickerCard } from "./PickerCard";
import { BootCard } from "./BootCard";
import { LoadErrorCard } from "./LoadErrorCard";
import { CrashCard } from "./CrashCard";

/// The exit fade length; the viewport reveal waits it out so the loaded scene fades in under the
/// dissolving launcher instead of popping in beside it.
const LAUNCHER_EXIT_MS = 300;

type Card = "picker" | "boot" | "loadError" | "crash";

export function Launcher() {
  const launcherOpen = useEditorStore((s) => s.launcherOpen);
  const hasProject = useEditorStore((s) => s.project !== null);
  const loadPhase = useEditorStore((s) => s.projectLoad.phase);
  const crashed = useEditorStore((s) => s.sessionCrash !== null);
  const setLauncherOpen = useEditorStore((s) => s.setLauncherOpen);
  const setViewportHidden = useEditorStore((s) => s.setViewportHidden);

  const visible =
    launcherOpen || !hasProject || loadPhase === "loading" || loadPhase === "error" || crashed;

  // Keep rendering through the exit fade, then unmount (the dot grid stops with it).
  const [rendered, setRendered] = useState(visible);
  useEffect(() => {
    if (visible) {
      setRendered(true);
      return;
    }
    const timer = setTimeout(() => setRendered(false), LAUNCHER_EXIT_MS);
    return () => clearTimeout(timer);
  }, [visible]);

  // Park the native viewport while the launcher owns the screen; reveal it only once the fade is
  // done, so the scene appears under the dissolving launcher.
  useEffect(() => {
    if (visible) {
      setViewportHidden(true);
      return;
    }
    const timer = setTimeout(() => setViewportHidden(false), LAUNCHER_EXIT_MS);
    return () => clearTimeout(timer);
  }, [visible, setViewportHidden]);

  // Over a live project (menu-opened picker) the launcher is dismissable: X or Escape.
  const dismissable = visible && hasProject && loadPhase === "idle" && !crashed;
  useEffect(() => {
    if (!dismissable) {
      return;
    }
    const onKey = (event: KeyboardEvent): void => {
      if (event.key === "Escape") {
        setLauncherOpen(false);
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [dismissable, setLauncherOpen]);

  // Freeze the card while fading out — completion flips the load phase a frame before the fade
  // starts, and the picker must not flash behind the dissolving boot card.
  const card: Card = crashed
    ? "crash"
    : loadPhase === "error"
      ? "loadError"
      : loadPhase === "loading"
        ? "boot"
        : "picker";
  const sticky = useRef(card);
  if (visible) {
    sticky.current = card;
  }
  const shown = sticky.current;

  if (!rendered) {
    return null;
  }
  return (
    <div
      className={cn(
        "fixed inset-0 z-50 flex flex-col bg-background text-foreground transition-opacity duration-300",
        visible ? "opacity-100" : "pointer-events-none opacity-0",
      )}
    >
      <LauncherTitlebar />
      <div className="relative flex min-h-0 flex-1 flex-col items-center justify-center gap-16">
        <DotGrid />
        {/* The wordmark sets the A's crossbar-less (Λ) — the closest glyph to an A with the
            bar removed that stays a plain string. */}
        <h1
          className="relative z-10 select-none text-5xl font-bold tracking-[0.45em] text-foreground/90"
          aria-hidden="true"
        >
          ΛNIMΛ
        </h1>
        <div className="relative z-10 w-[760px] max-w-[92vw] rounded-xl border border-border bg-card/90 p-6 shadow-2xl backdrop-blur-sm">
          {dismissable ? (
            <Button
              type="button"
              size="icon-xs"
              variant="ghost"
              className="absolute top-3 right-3"
              aria-label="Close"
              onClick={() => setLauncherOpen(false)}
            >
              <X />
            </Button>
          ) : null}
          {shown === "crash" ? (
            <CrashCard />
          ) : shown === "loadError" ? (
            <LoadErrorCard />
          ) : shown === "boot" ? (
            <BootCard />
          ) : (
            <PickerCard />
          )}
        </div>
      </div>
    </div>
  );
}
