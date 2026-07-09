import type { CSSProperties } from "react";

import { getCurrentWindow, type ResizeDirection } from "../shell";

/// Client-side resize handles for the borderless toplevel. The shell owns a decorationless winit
/// Wayland window (the React UI draws its own titlebar), so the compositor provides no server-side
/// resize edges — these thin strips at the window border grab a pointer-down and start an interactive
/// `xdg_toplevel.resize` (the resize counterpart to the titlebar's drag-to-move). Each strip carries
/// the matching resize cursor, so the border shows the affordance on hover.
const EDGE = 6;
const CORNER = 12;

const appWindow = getCurrentWindow();

function ResizeStrip({
  direction,
  className,
  style,
}: {
  direction: ResizeDirection;
  className: string;
  style: CSSProperties;
}) {
  return (
    <div
      aria-hidden="true"
      className={className}
      style={{ position: "fixed", zIndex: 200, ...style }}
      onPointerDown={(event) => {
        if (event.button !== 0) {
          return;
        }
        event.preventDefault();
        void appWindow.startResizeDragging(direction).catch(() => {});
      }}
    />
  );
}

export function WindowResizeFrame() {
  return (
    <>
      <ResizeStrip
        direction="north"
        className="cursor-ns-resize"
        style={{ top: 0, left: CORNER, right: CORNER, height: EDGE }}
      />
      <ResizeStrip
        direction="south"
        className="cursor-ns-resize"
        style={{ bottom: 0, left: CORNER, right: CORNER, height: EDGE }}
      />
      <ResizeStrip
        direction="west"
        className="cursor-ew-resize"
        style={{ left: 0, top: CORNER, bottom: CORNER, width: EDGE }}
      />
      <ResizeStrip
        direction="east"
        className="cursor-ew-resize"
        style={{ right: 0, top: CORNER, bottom: CORNER, width: EDGE }}
      />
      <ResizeStrip
        direction="north-west"
        className="cursor-nwse-resize"
        style={{ top: 0, left: 0, width: CORNER, height: CORNER }}
      />
      <ResizeStrip
        direction="north-east"
        className="cursor-nesw-resize"
        style={{ top: 0, right: 0, width: CORNER, height: CORNER }}
      />
      <ResizeStrip
        direction="south-west"
        className="cursor-nesw-resize"
        style={{ bottom: 0, left: 0, width: CORNER, height: CORNER }}
      />
      <ResizeStrip
        direction="south-east"
        className="cursor-nwse-resize"
        style={{ bottom: 0, right: 0, width: CORNER, height: CORNER }}
      />
    </>
  );
}
