/// The launcher's titlebar: the same surface as the editor's (height, colors, drag region,
/// window controls) without the tab strip — there is nothing to navigate before a project opens.
import { DRAG_REGION, IS_MACOS, NO_DRAG_REGION } from "../lib/platform";
import { WindowControls } from "../app/WindowTitlebar";

export function LauncherTitlebar() {
  return (
    <header
      className="relative z-20 flex h-9 flex-none items-center border-b border-border bg-card"
      style={DRAG_REGION}
    >
      {/* macOS draws native traffic lights at the top-left; keep the bar clear of them. */}
      {IS_MACOS && <div className="w-20 flex-none self-stretch" />}
      <div className="min-w-0 flex-1 self-stretch" />
      {!IS_MACOS && (
        <div className="flex w-33 flex-none justify-end" style={NO_DRAG_REGION}>
          <WindowControls />
        </div>
      )}
    </header>
  );
}
