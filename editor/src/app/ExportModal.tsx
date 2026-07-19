/// The "Export App" dialog: gathers the app manifest (title + window + present options) and an
/// output path, then drives the engine's `export-app` cook over the control plane. The engine
/// pre-bakes shaders and stages `saffron-player` + project data in the native platform layout.
import { useEffect, useMemo, useState } from "react";
import { open } from "../shell";
import { FolderOpen, Package } from "lucide-react";
import { client } from "../control/client";
import { useEditorStore, withNativeDialog } from "../state/store";
import { errorText, notify, notifyError } from "../lib/flash";
import { IS_MACOS } from "../lib/platform";
import { Button } from "@/components/ui/button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Switch } from "@/components/ui/switch";

export function ExportModal() {
  const exportModalOpen = useEditorStore((s) => s.exportModalOpen);
  const setExportModalOpen = useEditorStore((s) => s.setExportModalOpen);
  const setViewportHidden = useEditorStore((s) => s.setViewportHidden);
  const project = useEditorStore((s) => s.project);

  const [title, setTitle] = useState("");
  // The user browses to a parent directory; export creates a platform-native child inside it.
  const [parentDir, setParentDir] = useState("");
  const [width, setWidth] = useState(1280);
  const [height, setHeight] = useState(720);
  const [fullscreen, setFullscreen] = useState(false);
  const [vsync, setVsync] = useState(true);
  const [status, setStatus] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  // The reparented viewport paints over the webview; park it while the modal is open.
  useEffect(() => {
    setViewportHidden(exportModalOpen);
    return () => setViewportHidden(false);
  }, [exportModalOpen, setViewportHidden]);

  // Seed the title from the project's display name each time the dialog opens.
  useEffect(() => {
    if (exportModalOpen) {
      setTitle((current) =>
        current.length > 0 ? current : (project?.displayName ?? "Saffron App"),
      );
      setStatus(null);
    }
  }, [exportModalOpen, project]);

  // The app is named after the title (filesystem-sanitized); macOS adds the bundle extension.
  const folderName = useMemo(() => sanitizeFolderName(title), [title]);
  const outputName = IS_MACOS ? `${folderName}.app` : folderName;
  const outputDir = parentDir ? `${parentDir.replace(/\/+$/, "")}/${outputName}` : "";

  const pickParentDir = async (): Promise<void> => {
    const selection = await withNativeDialog(() => open({ directory: true, multiple: false }));
    if (typeof selection === "string") {
      setParentDir(selection);
    }
  };

  const canExport =
    title.trim().length > 0 &&
    parentDir.length > 0 &&
    folderName.length > 0 &&
    width > 0 &&
    height > 0;

  const exportApp = async (): Promise<void> => {
    if (!canExport) {
      setStatus("Set an app title and choose a destination folder.");
      return;
    }
    setBusy(true);
    setStatus(null);
    try {
      const result = await client.exportApp(outputDir, {
        title: title.trim(),
        width,
        height,
        fullscreen,
        vsync,
      });
      for (const warning of result.warnings) {
        notifyError(`Export warning: ${warning}`);
      }
      notify(`Exported app to ${result.path}`);
      setExportModalOpen(false);
    } catch (err) {
      setStatus(errorText(err));
      notifyError(errorText(err));
    } finally {
      setBusy(false);
    }
  };

  return (
    <Dialog
      open={exportModalOpen}
      onOpenChange={(next) => {
        if (!next && !busy) {
          setExportModalOpen(false);
        }
      }}
    >
      <DialogContent showCloseButton={!busy} className="sm:max-w-[480px]">
        <DialogHeader>
          <DialogTitle>Export App</DialogTitle>
          <DialogDescription>
            Cook the project into a standalone, runnable application.
          </DialogDescription>
        </DialogHeader>

        <div className="space-y-4">
          <div className="space-y-2">
            <Label htmlFor="export-title">App title</Label>
            <Input
              id="export-title"
              value={title}
              onChange={(event) => setTitle(event.target.value)}
              placeholder="My App"
              disabled={busy}
            />
          </div>

          <div className="space-y-2">
            <Label htmlFor="export-output">Destination folder</Label>
            <div className="flex gap-2">
              <Input
                id="export-output"
                value={outputDir}
                readOnly
                placeholder="Browse to a parent folder…"
                disabled={busy}
                className="min-w-0 flex-1 font-mono text-xs"
              />
              <Button
                type="button"
                variant="outline"
                onClick={() => void pickParentDir()}
                disabled={busy}
              >
                <FolderOpen />
                Browse
              </Button>
            </div>
          </div>

          <div className="grid grid-cols-2 gap-3">
            <div className="space-y-2">
              <Label htmlFor="export-width">Width</Label>
              <Input
                id="export-width"
                type="number"
                min={1}
                value={width}
                onChange={(event) => setWidth(Math.max(1, Number(event.target.value) || 0))}
                disabled={busy}
              />
            </div>
            <div className="space-y-2">
              <Label htmlFor="export-height">Height</Label>
              <Input
                id="export-height"
                type="number"
                min={1}
                value={height}
                onChange={(event) => setHeight(Math.max(1, Number(event.target.value) || 0))}
                disabled={busy}
              />
            </div>
          </div>

          <div className="flex items-center justify-between">
            <Label htmlFor="export-fullscreen">Start fullscreen</Label>
            <Switch
              id="export-fullscreen"
              checked={fullscreen}
              onCheckedChange={setFullscreen}
              disabled={busy}
            />
          </div>
          <div className="flex items-center justify-between">
            <Label htmlFor="export-vsync">VSync</Label>
            <Switch id="export-vsync" checked={vsync} onCheckedChange={setVsync} disabled={busy} />
          </div>

          <p className="text-xs text-muted-foreground">
            Target: {IS_MACOS ? "macOS application bundle" : "Linux (x86_64)"}
          </p>
          {status ? <p className="text-xs text-destructive">{status}</p> : null}
        </div>

        <DialogFooter>
          <Button
            type="button"
            variant="outline"
            onClick={() => setExportModalOpen(false)}
            disabled={busy}
          >
            Cancel
          </Button>
          <Button type="button" onClick={() => void exportApp()} disabled={busy || !canExport}>
            <Package />
            {busy ? "Exporting…" : "Export"}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}

/// A filesystem-safe folder name from the app title: path separators collapse to spaces, runs of
/// whitespace (incl. tabs/newlines) collapse, and it trims — falling back to "app".
function sanitizeFolderName(title: string): string {
  const cleaned = title
    .replace(/[/\\]+/g, " ")
    .replace(/\s+/g, " ")
    .trim();
  return cleaned.length > 0 ? cleaned : "app";
}
