/// The Profiler capture controls: a single Start/Stop toggle, the window-length selector, and
/// Download. Arming a capture forces the engine's timestamps mode + sub-scopes for the duration
/// (restored on stop), so a capture never silently leaves the baseline host instrumented — the
/// panel surfaces that honestly. Progress is driven by polling the non-destructive
/// `capture-status` while recording; draining (`capture-stop`) happens only once ready.
import { useCallback, useEffect, useRef, useState } from "react";
import { ChevronDown, Download, ExternalLink, Flame } from "lucide-react";
import { invoke } from "@tauri-apps/api/core";
import { save } from "@tauri-apps/plugin-dialog";
import { client } from "../control/client";
import { useEditorStore } from "../state/store";
import { captureToChromeTrace } from "../lib/chromeTrace";
import { errorText, notify, notifyError } from "../lib/flash";
import { PERFETTO_URL, toPerfettoTrace } from "../lib/perfettoExport";
import { Button } from "@/components/ui/button";
import { ButtonGroup } from "@/components/ui/button-group";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuSub,
  DropdownMenuSubContent,
  DropdownMenuSubTrigger,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import { Tooltip, TooltipContent, TooltipTrigger } from "@/components/ui/tooltip";

const WINDOW_PRESETS = [1, 8, 64, 256];

export function CaptureControls() {
  const captureState = useEditorStore((s) => s.captureState);
  const captureProgress = useEditorStore((s) => s.captureProgress);
  const captureWindowFrames = useEditorStore((s) => s.captureWindowFrames);
  const captureIncludeStats = useEditorStore((s) => s.captureIncludeStats);
  const capture = useEditorStore((s) => s.capture);
  const setCaptureState = useEditorStore((s) => s.setCaptureState);
  const setCaptureProgress = useEditorStore((s) => s.setCaptureProgress);
  const setCapture = useEditorStore((s) => s.setCapture);
  const setCaptureWindowFrames = useEditorStore((s) => s.setCaptureWindowFrames);
  const setSelectedPass = useEditorStore((s) => s.setSelectedPass);
  const openFlameTab = useEditorStore((s) => s.openFlameTab);
  const [statsSupported, setStatsSupported] = useState(false);
  const [barVisible, setBarVisible] = useState(false);
  const captureStartedAtRef = useRef(0);

  const recording = captureState === "recording" || captureState === "arming";

  // Guards the destructive `capture-stop` to exactly one call per capture. The poll loop and a
  // manual Stop both drain through `drainCapture`; whichever wins flips this, so a second poll
  // tick (control round-trips can exceed the poll interval when frames are slow, so ticks
  // overlap) cannot re-stop an already-drained recorder and clobber the result with an empty one.
  const drainingRef = useRef(false);

  // Single-flight drain: stop the capture once, keep the result only when the engine actually
  // recorded frames. A redundant or empty stop never nulls a good capture.
  const drainCapture = useCallback(async (): Promise<void> => {
    if (drainingRef.current) {
      return;
    }
    drainingRef.current = true;
    try {
      const result = await client.captureStop();
      if (result.ready) {
        setCapture(result.capture);
      }
    } catch (err) {
      notifyError(`Capture failed: ${errorText(err)}`);
    }
    // Always leave recording, even if draining failed, so the controls never freeze.
    setCaptureState("ready");
  }, [setCapture, setCaptureState]);

  // Learn the pipeline-statistics capability once (capture-status is non-destructive), so the
  // stats toggle can be disabled when the device lacks pipelineStatisticsQuery.
  useEffect(() => {
    let live = true;
    void client
      .captureStatus()
      .then((status) => {
        if (live) {
          setStatsSupported(status.pipelineStatsSupported);
        }
      })
      .catch(() => {});
    return () => {
      live = false;
    };
  }, []);

  // Poll the engine's capture progress while recording; drain once it reports ready. The effect
  // is keyed on captureState so it also resumes if the panel re-mounts mid-capture, and tears the
  // interval down when recording ends.
  useEffect(() => {
    if (!recording) {
      return;
    }
    const id = window.setInterval(() => {
      void (async () => {
        let status;
        try {
          status = await client.captureStatus();
        } catch {
          return;
        }
        setCaptureProgress(status.capturedFrames, status.targetFrames);
        if (status.state === "ready") {
          clearInterval(id);
          await drainCapture();
        } else if (status.state === "idle") {
          // Already drained (a manual Stop or an earlier overlapping tick) — stop polling
          // without a second capture-stop, which would return an empty capture and null the
          // result. Leave recording so the controls settle.
          clearInterval(id);
          setCaptureState("ready");
        }
      })();
    }, 150);
    return () => clearInterval(id);
  }, [recording, drainCapture, setCaptureProgress, setCaptureState]);

  // Keep the progress bar on screen for at least a second after a capture starts, even if it
  // finishes instantly (a single frame) — it animates to full, lingers, then clears, so a fast
  // capture is still legible instead of a sub-frame flash.
  useEffect(() => {
    if (recording) {
      setBarVisible(true);
      return;
    }
    if (!barVisible) {
      return;
    }
    const remaining = Math.max(0, 1000 - (Date.now() - captureStartedAtRef.current));
    const id = window.setTimeout(() => setBarVisible(false), remaining);
    return () => clearTimeout(id);
  }, [recording, barVisible]);

  const start = async (): Promise<void> => {
    captureStartedAtRef.current = Date.now();
    drainingRef.current = false;
    setSelectedPass(null);
    setCapture(null);
    setCaptureProgress(0, captureWindowFrames);
    setCaptureState("recording");
    const mode = captureWindowFrames <= 1 ? "single" : "frames";
    try {
      await client.captureStart({
        mode,
        frames: captureWindowFrames,
        includePipelineStats: captureIncludeStats && statsSupported,
      });
    } catch (err) {
      notifyError(`Capture failed: ${errorText(err)}`);
      setCaptureState("idle");
    }
  };

  const stopNow = async (): Promise<void> => {
    await drainCapture();
  };

  const onToggle = (): void => {
    if (recording) {
      void stopNow();
    } else {
      void start();
    }
  };

  // Save trace bytes to a user-chosen path via the Tauri save dialog + bridge write (the webview
  // cannot `<a download>` a blob). Bytes go over the bridge as a plain number array.
  const saveTrace = async (
    defaultName: string,
    ext: string,
    label: string,
    bytes: Uint8Array,
  ): Promise<void> => {
    try {
      const path = await save({
        defaultPath: defaultName,
        filters: [{ name: label, extensions: [ext] }],
      });
      if (path === null) {
        return; // user cancelled
      }
      await invoke("write_file", { path, bytes: Array.from(bytes) });
      notify(`Saved ${path}`);
    } catch (err) {
      notifyError(`Export failed: ${errorText(err)}`);
    }
  };

  const onDownloadJson = (): void => {
    if (capture !== null) {
      void saveTrace(
        "saffron-profile.json",
        "json",
        "Chrome Trace",
        new TextEncoder().encode(captureToChromeTrace(capture)),
      );
    }
  };

  const onDownloadPerfetto = (): void => {
    if (capture !== null) {
      void saveTrace(
        "saffron-profile.perfetto-trace",
        "perfetto-trace",
        "Perfetto Trace",
        toPerfettoTrace(capture),
      );
    }
  };

  // Auto-import into Perfetto: stash the trace on the bridge's loopback server, then open
  // ui.perfetto.dev with `?url=` pointing at it so Perfetto fetches and loads it itself — no
  // download/drag (the postMessage handoff can't cross the webview → desktop-browser boundary).
  const onOpenPerfetto = (): void => {
    if (capture === null) {
      return;
    }
    void (async () => {
      try {
        const url = await invoke<string>("serve_trace", {
          bytes: Array.from(toPerfettoTrace(capture)),
        });
        const link = `${PERFETTO_URL}/#!/?url=${encodeURIComponent(url)}`;
        await invoke("open_external", { url: link });
      } catch (err) {
        notifyError(`Open failed: ${errorText(err)}`);
      }
    })();
  };

  const progressFrac =
    captureProgress.total > 0 ? Math.min(1, captureProgress.current / captureProgress.total) : 0;

  return (
    <div className="@container flex flex-col gap-1.5">
      <div className="flex items-center gap-2">
        {/* The capture control: the primary Capture/Stop button joined with a frame-count Select. */}
        <ButtonGroup>
          <Button
            size="sm"
            variant={recording ? "destructive" : "default"}
            className="gap-1.5"
            onClick={onToggle}
          >
            {recording ? (
              <>
                <span className="size-2 rounded-[1px] bg-current" aria-hidden />
                Stop ({captureProgress.current}/{captureProgress.total})
              </>
            ) : (
              <>
                <span className="size-2 rounded-full bg-red-500" aria-hidden />
                Capture
              </>
            )}
          </Button>
          <Select
            value={String(captureWindowFrames)}
            disabled={recording}
            onValueChange={(value) => setCaptureWindowFrames(Number(value))}
          >
            <SelectTrigger
              size="sm"
              aria-label="Capture frame count"
              className="w-auto gap-1 border-transparent bg-secondary px-2.5 font-mono text-[11px] tabular-nums text-secondary-foreground shadow-xs hover:bg-secondary/80 dark:bg-secondary dark:hover:bg-secondary/80"
            >
              <SelectValue />
            </SelectTrigger>
            <SelectContent align="end">
              {WINDOW_PRESETS.map((frames) => (
                <SelectItem
                  key={frames}
                  value={String(frames)}
                  className="font-mono text-[11px] tabular-nums"
                >
                  {frames}
                </SelectItem>
              ))}
            </SelectContent>
          </Select>
        </ButtonGroup>

        {/* Wide: three icon actions. Narrow (thin sidebar): collapse to a single chevron menu. */}
        <div className="ml-auto hidden items-center gap-1 @min-[280px]:flex">
          <Tooltip>
            <TooltipTrigger asChild>
              <Button
                size="icon-sm"
                variant="outline"
                disabled={capture === null}
                onClick={() => openFlameTab()}
                aria-label="Open flame graph"
              >
                <Flame />
              </Button>
            </TooltipTrigger>
            <TooltipContent>Open flame graph</TooltipContent>
          </Tooltip>
          <DropdownMenu>
            <Tooltip>
              <TooltipTrigger asChild>
                <DropdownMenuTrigger asChild>
                  <Button
                    size="icon-sm"
                    variant="outline"
                    disabled={capture === null}
                    aria-label="Download"
                  >
                    <Download />
                  </Button>
                </DropdownMenuTrigger>
              </TooltipTrigger>
              <TooltipContent>Download</TooltipContent>
            </Tooltip>
            {/* Don't return focus to the trigger on close — it shares the Tooltip trigger, and a
                focus-reopened tooltip would stick with no pointer present to dismiss it. */}
            <DropdownMenuContent align="end" onCloseAutoFocus={(e) => e.preventDefault()}>
              <DropdownMenuItem onSelect={onDownloadJson}>JSON (Chrome Trace)</DropdownMenuItem>
              <DropdownMenuItem onSelect={onDownloadPerfetto}>Perfetto</DropdownMenuItem>
            </DropdownMenuContent>
          </DropdownMenu>
          <Tooltip>
            <TooltipTrigger asChild>
              <Button
                size="icon-sm"
                variant="outline"
                disabled={capture === null}
                onClick={onOpenPerfetto}
                aria-label="Open in Perfetto"
              >
                <ExternalLink />
              </Button>
            </TooltipTrigger>
            <TooltipContent>Open in Perfetto</TooltipContent>
          </Tooltip>
        </div>

        <div className="ml-auto flex @min-[280px]:hidden">
          <DropdownMenu>
            <DropdownMenuTrigger asChild>
              <Button
                size="icon-sm"
                variant="outline"
                disabled={capture === null}
                aria-label="Trace actions"
              >
                <ChevronDown />
              </Button>
            </DropdownMenuTrigger>
            <DropdownMenuContent align="end">
              <DropdownMenuItem onSelect={() => openFlameTab()}>
                <Flame className="size-3.5" />
                Open flame graph
              </DropdownMenuItem>
              <DropdownMenuSub>
                <DropdownMenuSubTrigger>
                  <Download className="size-3.5" />
                  Download
                </DropdownMenuSubTrigger>
                <DropdownMenuSubContent>
                  <DropdownMenuItem onSelect={onDownloadJson}>JSON (Chrome Trace)</DropdownMenuItem>
                  <DropdownMenuItem onSelect={onDownloadPerfetto}>Perfetto</DropdownMenuItem>
                </DropdownMenuSubContent>
              </DropdownMenuSub>
              <DropdownMenuItem onSelect={onOpenPerfetto}>
                <ExternalLink className="size-3.5" />
                Open in Perfetto
              </DropdownMenuItem>
            </DropdownMenuContent>
          </DropdownMenu>
        </div>
      </div>

      {barVisible ? (
        <div className="h-1 w-full overflow-hidden rounded-full bg-white/10">
          <div
            className="h-full rounded-full bg-red-500/80 transition-[width] duration-300 ease-out"
            style={{ width: `${recording ? progressFrac * 100 : 100}%` }}
          />
        </div>
      ) : null}
    </div>
  );
}
