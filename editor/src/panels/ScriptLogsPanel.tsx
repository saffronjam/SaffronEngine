/// The Script Logs diagnostics panel: sa.log output drained from the open-AND-playing poll in store.ts,
/// shown as `<time> [<entity>] <message>` rows. Search is a VS Code-style find widget: hidden until Ctrl+F
/// (while the panel is active) reveals it as a top-right overlay, Esc or its X closes it. The bar is
/// AnimaSearchbar with an `Entity:` chip (typed-verb autocomplete over the scene's entities) plus free-text
/// message filtering. The list is windowed (only visible rows mount, each memoized) and auto-scrolls to
/// newest unless the user has scrolled up to read history.
import * as React from "react";
import { useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";
import { TriangleAlert, X } from "lucide-react";

import { useEditorStore } from "../state/store";
import { AnimaSearchbar } from "../components/anima/AnimaSearchbar";
import {
  emptySearchState,
  type ChipConfig,
  type SearchState,
} from "../components/anima/chipSearch";
import { Button } from "@/components/ui/button";
import { cn } from "@/lib/utils";

const ROW_HEIGHT = 20; // px — uniform single-line font-mono rows, so the window needs no measurement
const OVERSCAN = 8;

function shortId(id: string): string {
  return id.length > 8 ? `…${id.slice(-6)}` : id;
}

function formatTime(epochMs: number): string {
  const d = new Date(epochMs);
  const p = (n: number, w = 2): string => String(n).padStart(w, "0");
  return `${p(d.getHours())}:${p(d.getMinutes())}:${p(d.getSeconds())}.${p(d.getMilliseconds(), 3)}`;
}

const LogRow = React.memo(function LogRow({
  time,
  entityLabel,
  noEntity,
  message,
  top,
}: {
  time: string;
  entityLabel: string;
  noEntity: boolean;
  message: string;
  top: number;
}) {
  return (
    <div
      className="absolute right-0 left-0 flex items-baseline gap-2 px-2"
      style={{ top, height: ROW_HEIGHT }}
    >
      <span className="shrink-0 tabular-nums text-muted-foreground">{time}</span>
      <span className={cn("shrink-0 text-sky-400", noEntity && "text-muted-foreground")}>
        [{entityLabel}]
      </span>
      <span className="min-w-0 flex-1 truncate text-foreground">{message}</span>
    </div>
  );
});

export function ScriptLogsPanel() {
  const playState = useEditorStore((s) => s.playState);
  const playing = playState !== "edit";
  const scriptLogs = useEditorStore((s) => s.scriptLogs);
  const overflowed = useEditorStore((s) => s.scriptLogsOverflowed);
  const entities = useEditorStore((s) => s.entities);

  const nameById = useMemo(() => {
    const m = new Map<string, string>();
    for (const e of entities) m.set(e.id, e.name);
    return m;
  }, [entities]);
  const label = (id: string): string => (id === "0" ? "—" : (nameById.get(id) ?? shortId(id)));

  const [searchOpen, setSearchOpen] = useState(false);
  const [search, setSearch] = useState<SearchState>(emptySearchState());
  const searchInputRef = useRef<HTMLInputElement | null>(null);

  const closeSearch = (): void => {
    setSearchOpen(false);
    setSearch(emptySearchState());
  };

  // The `Entity:` verb autocompletes scene entity names; the committed chip carries the entity id and
  // resolves back to the name (or a short id when the entity was deleted).
  const entityChip: ChipConfig = useMemo(
    () => ({
      keyword: "Entity",
      label: "Entity",
      options: (input) => {
        const q = input.toLowerCase();
        return entities
          .filter((e) => !q || e.name.toLowerCase().includes(q))
          .slice(0, 20)
          .map((e) => ({ value: e.id, label: e.name }));
      },
      resolveLabel: (id) => nameById.get(id) ?? shortId(id),
    }),
    [entities, nameById],
  );

  // Entity chips OR-group by id (any selected entity); free text is a case-insensitive message
  // substring AND-ed on top. This is the one bar that replaces free-text-vs-filter-buttons.
  const filtered = useMemo(() => {
    const entityIds = new Set(
      search.chips.filter((c) => c.keyword === "Entity").map((c) => c.value),
    );
    const free = search.freeText.trim().toLowerCase();
    if (entityIds.size === 0 && !free) return scriptLogs;
    return scriptLogs.filter(
      (l) =>
        (entityIds.size === 0 || entityIds.has(l.entity)) &&
        (!free || l.message.toLowerCase().includes(free)),
    );
  }, [scriptLogs, search]);

  // Windowed virtualization over the filtered array.
  const scrollRef = useRef<HTMLDivElement | null>(null);
  const stickToBottom = useRef(true);
  const [scrollTop, setScrollTop] = useState(0);
  const [viewportH, setViewportH] = useState(0);

  useEffect(() => {
    const el = scrollRef.current;
    if (!el) return;
    const ro = new ResizeObserver(() => setViewportH(el.clientHeight));
    ro.observe(el);
    setViewportH(el.clientHeight);
    return () => ro.disconnect();
  }, [playing]);

  // Pin to newest on append while the user is at the bottom — synchronously before paint, and seed the
  // window's scrollTop in the same pass, so the visible slice never lags the jump (which would flash empty).
  useLayoutEffect(() => {
    const el = scrollRef.current;
    if (el && stickToBottom.current) {
      el.scrollTop = el.scrollHeight;
      setScrollTop(el.scrollTop);
    }
  }, [filtered.length]);

  // Focus the find input whenever the widget opens (or is re-summoned with Ctrl+F).
  useEffect(() => {
    if (searchOpen) requestAnimationFrame(() => searchInputRef.current?.focus());
  }, [searchOpen]);

  const onScroll = (e: React.UIEvent<HTMLDivElement>): void => {
    const el = e.currentTarget;
    setScrollTop(el.scrollTop);
    stickToBottom.current = el.scrollHeight - el.clientHeight - el.scrollTop < ROW_HEIGHT * 2;
  };

  const onKeyDown = (e: React.KeyboardEvent<HTMLDivElement>): void => {
    if ((e.ctrlKey || e.metaKey) && (e.key === "f" || e.key === "F")) {
      e.preventDefault();
      setSearchOpen(true); // open-only — never toggles closed
      requestAnimationFrame(() => searchInputRef.current?.focus());
      return;
    }
    if (e.key === "Escape" && searchOpen) {
      e.preventDefault();
      closeSearch();
    }
  };

  const total = filtered.length;
  const start = Math.max(0, Math.floor(scrollTop / ROW_HEIGHT) - OVERSCAN);
  const end = Math.min(total, Math.ceil((scrollTop + viewportH) / ROW_HEIGHT) + OVERSCAN);
  const visible = filtered.slice(start, end);

  return (
    <div
      className="relative flex h-full min-h-0 flex-col outline-none"
      tabIndex={0}
      onKeyDown={onKeyDown}
    >
      {overflowed ? (
        <div className="flex shrink-0 items-center gap-1.5 border-b border-amber-500/25 bg-amber-500/10 px-2 py-1 text-[10px] text-amber-300">
          <TriangleAlert className="size-3 shrink-0" />
          <span>A script is logging faster than the editor can keep up.</span>
        </div>
      ) : null}

      {!playing && scriptLogs.length === 0 ? (
        <p className="p-3 text-[11px] text-muted-foreground italic">
          Enter Play to see script logs.
        </p>
      ) : (
        <div
          ref={scrollRef}
          onScroll={onScroll}
          tabIndex={0}
          className="min-h-0 flex-1 overflow-auto font-mono text-[11px] outline-none select-text"
        >
          {total === 0 ? (
            <p className="p-3 text-muted-foreground italic">
              {scriptLogs.length === 0 ? "No logs yet." : "No logs match the filter."}
            </p>
          ) : (
            <div style={{ height: total * ROW_HEIGHT, position: "relative" }}>
              {visible.map((l, i) => (
                <LogRow
                  key={l.seq}
                  time={formatTime(l.epochMs)}
                  entityLabel={label(l.entity)}
                  noEntity={l.entity === "0"}
                  message={l.message}
                  top={(start + i) * ROW_HEIGHT}
                />
              ))}
            </div>
          )}
        </div>
      )}

      {/* Find widget: a top-right overlay revealed by Ctrl+F, dismissed by Esc or its X. */}
      {searchOpen ? (
        <div className="absolute top-1.5 right-1.5 z-20 flex w-72 max-w-[calc(100%-0.75rem)] items-center gap-1 rounded-md border border-border bg-card p-1 shadow-lg">
          <AnimaSearchbar
            value={search}
            onChange={setSearch}
            chips={[entityChip]}
            placeholder="Find"
            debounceMs={120}
            inputRef={searchInputRef}
            showClear={false}
            className="flex-1"
          />
          <Button
            type="button"
            variant="ghost"
            size="icon"
            className="h-6 w-6 shrink-0"
            onClick={closeSearch}
            aria-label="Close find"
          >
            <X className="size-4" />
          </Button>
        </div>
      ) : null}
    </div>
  );
}
