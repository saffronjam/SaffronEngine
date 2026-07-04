// The Store results grid: a windowed grid of result cards fed by infinite scroll. The selected
// store advances its cursor server-side; we pull the next batch as the user nears the end and stop
// when the session reports the store exhausted. Results/scroll live in the Zustand store keyed by
// the active session, so reopening the Store restores them without refetching.
import * as React from "react";
import { useCallback, useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { useVirtualizer } from "@tanstack/react-virtual";
import { ExternalLink, Loader2, Maximize2 } from "lucide-react";

import { Badge } from "@/components/ui/badge";
import { Tooltip, TooltipContent, TooltipTrigger } from "@/components/ui/tooltip";

import { errorText, notifyError } from "../lib/flash";
import { useEditorStore } from "../state/store";
import { AssetDetailModal } from "./AssetDetailModal";
import { GalleryViewer } from "./GalleryViewer";
import { ImportControls } from "./ImportControls";
import { storeSearchMore, type StoreResult } from "./types";
import { useGallery, useGalleryNav } from "./useGallery";

const CELL_W = 196; // px — tile + gap
const CELL_H = 232;
const OVERSCAN_ROWS = 3;
// Pulled per infinite-scroll refill. Large enough that a wide window fills in a couple of round
// trips (a batch is spread across ~10+ columns), then scrolling pulls the rest.
const BATCH = 48;

export function StoreResultsGrid({
  session,
  active,
  onLoadingChange,
}: {
  session: string;
  active: boolean;
  onLoadingChange?: (loading: boolean) => void;
}) {
  const results = useEditorStore((s) => s.storeResults);
  const exhausted = useEditorStore((s) => s.storeExhausted);
  const [loading, setLoading] = useState(false);

  const scrollRef = useRef<HTMLDivElement | null>(null);
  const [viewport, setViewport] = useState({ w: 0, h: 0 });
  // Guards a refill against the session/state captured when it started.
  const loadingRef = useRef(false);
  // On a new session keep the old results visible until the first new batch arrives, then
  // replace — so a re-search shows the searchbar spinner over existing results, not a blank flash.
  const pendingReset = useRef(false);

  const loadMore = useCallback(() => {
    if (loadingRef.current || exhausted) return;
    loadingRef.current = true;
    setLoading(true);
    storeSearchMore(session, BATCH)
      .then((page) => {
        const reset = pendingReset.current;
        pendingReset.current = false;
        const store = useEditorStore.getState();
        if (reset) store.setStoreResults(page.results, session);
        else store.appendStoreResults(page.results, session);
        store.setStoreExhausted(page.exhausted);
      })
      .catch((err: unknown) => {
        useEditorStore.getState().setStoreExhausted(true);
        notifyError(errorText(err));
      })
      .finally(() => {
        loadingRef.current = false;
        setLoading(false);
      });
  }, [session, exhausted]);

  // Surface the in-flight state so the host can show the searchbar spinner; reset it if the
  // grid unmounts mid-load (e.g. switching to the credits view).
  useEffect(() => {
    onLoadingChange?.(loading);
    return () => onLoadingChange?.(false);
  }, [loading, onLoadingChange]);

  // A fresh session reloads from the top; a remount whose results already belong to this session
  // (e.g. reopening the Store tab) restores the scroll position and skips the refetch.
  useEffect(() => {
    const store = useEditorStore.getState();
    if (store.storeResultsSession === session && (store.storeResults.length > 0 || store.storeExhausted)) {
      if (scrollRef.current) scrollRef.current.scrollTop = store.storeScrollTop;
      return;
    }
    store.setStoreExhausted(false);
    if (scrollRef.current) scrollRef.current.scrollTop = 0;
    store.setStoreScrollTop(0);
    loadingRef.current = false;
    setLoading(false);
    pendingReset.current = true;
    // Defer to let state reset settle before the first pull.
    const id = requestAnimationFrame(() => loadMore());
    return () => cancelAnimationFrame(id);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [session]);

  useEffect(() => {
    const el = scrollRef.current;
    if (!el) return;
    // Ignore zero-size observations: while the Store tab is hidden (display:none) the element
    // measures 0×0, which would collapse the windowed set to ~2 rows and make it re-expand on
    // reveal. Keeping the last good size renders the cached grid instantly when the tab returns.
    const measure = () => {
      if (el.clientWidth > 0 && el.clientHeight > 0) {
        setViewport({ w: el.clientWidth, h: el.clientHeight });
      }
    };
    const ro = new ResizeObserver(measure);
    ro.observe(el);
    measure();
    return () => ro.disconnect();
  }, []);

  const columns = Math.max(1, Math.floor((viewport.w || CELL_W) / CELL_W));
  const rowCount = Math.ceil(results.length / columns);

  // Row virtualization via @tanstack/react-virtual: it re-renders only when the visible row range
  // changes — not on every scroll pixel — so the grid stops lagging the native scroll (the source
  // of the earlier tearing/ripple). Rows are a fixed CELL_H, so the size estimate is exact; each
  // virtual row lays out its `columns` cards.
  const rowVirtualizer = useVirtualizer({
    count: rowCount,
    getScrollElement: () => scrollRef.current,
    estimateSize: () => CELL_H,
    overscan: OVERSCAN_ROWS,
  });
  const totalHeight = rowVirtualizer.getTotalSize();

  // Fill the viewport: keep pulling until the content overflows (plus overscan) or the store is
  // exhausted, so a first batch that doesn't cover the visible area can't leave the grid stuck.
  useEffect(() => {
    if (exhausted || loading || viewport.h <= 0) return;
    if (totalHeight < viewport.h + CELL_H * (OVERSCAN_ROWS + 1)) {
      loadMore();
    }
  }, [totalHeight, viewport.h, exhausted, loading, loadMore]);

  const onScroll = (e: React.UIEvent<HTMLDivElement>) => {
    const el = e.currentTarget;
    // Persist scroll for a reopen (no React state here → no per-event re-render; the virtualizer
    // owns visible-range updates). Pull the next batch as the end nears.
    useEditorStore.getState().setStoreScrollTop(el.scrollTop);
    if (el.scrollHeight - el.clientHeight - el.scrollTop < CELL_H * 3) {
      loadMore();
    }
  };

  return (
    <div className="relative min-h-0 flex-1">
      <div ref={scrollRef} onScroll={onScroll} className="absolute inset-0 overflow-auto p-2">
        {results.length === 0 && !loading ? (
          <p className="p-8 text-center text-sm text-muted-foreground italic">
            Nothing here — try a different search.
          </p>
        ) : (
          <div style={{ height: totalHeight, position: "relative", width: "100%" }}>
            {rowVirtualizer.getVirtualItems().map((virtualRow) => (
              <div
                key={virtualRow.key}
                style={{
                  position: "absolute",
                  top: 0,
                  left: 0,
                  width: "100%",
                  height: CELL_H,
                  transform: `translateY(${virtualRow.start}px)`,
                }}
              >
                {Array.from({ length: columns }, (_, col) => {
                  const index = virtualRow.index * columns + col;
                  if (index >= results.length) return null;
                  const result = results[index];
                  return (
                    <StoreCard
                      key={`${result.store.id}:${result.id}`}
                      result={result}
                      active={active}
                      left={col * CELL_W}
                    />
                  );
                })}
              </div>
            ))}
          </div>
        )}
        {exhausted && results.length > 0 ? (
          <p className="py-3 text-center text-xs text-muted-foreground italic">End of results.</p>
        ) : null}
      </div>
      {/* Whole-store overlay spinner while loading with nothing to show yet (initial / a
          fresh search); an incremental load over existing results uses the searchbar spinner. */}
      {loading && results.length === 0 ? (
        <div className="pointer-events-none absolute inset-0 flex items-center justify-center">
          <Loader2 className="size-8 animate-spin text-muted-foreground" />
        </div>
      ) : null}
    </div>
  );
}

const StoreCard = React.memo(function StoreCard({
  result,
  active,
  left,
}: {
  result: StoreResult;
  active: boolean;
  left: number;
}) {
  const [expanded, setExpanded] = useState(false);
  // The gallery is fetched lazily — once the card is hovered or the modal is opened — so a
  // scroll past a hundred cards doesn't fire a hundred provider requests.
  const [hovered, setHovered] = useState(false);
  const { images } = useGallery(result, hovered || expanded);
  // The card and the modal share one fetch but navigate independently.
  const nav = useGalleryNav(images.length);

  return (
    // Paint containment isolates each card's rasterization: an animation of a compositing
    // property inside one card (the gallery's slide transform, an overlay fade) confines its
    // repaint to this box and can't re-rasterize sibling cards — whose text would otherwise flip
    // antialiasing on the software-composited webview, reading as a font-size shimmer. It's a
    // containment boundary, not a compositing layer (unlike translateZ, which promotes a layer per
    // card and stalls the software compositor), so it stays cheap.
    <div
      className="group absolute flex flex-col overflow-hidden rounded-md border border-border bg-card [contain:paint]"
      style={{ top: 0, left, width: CELL_W - 12, height: CELL_H - 12 }}
      onMouseEnter={() => setHovered(true)}
    >
      <div className="relative h-28 w-full shrink-0 bg-muted">
        <GalleryViewer images={images} nav={nav} alt={result.name} />
        <button
          type="button"
          aria-label="Expand"
          onClick={() => setExpanded(true)}
          className="absolute top-1 right-1 flex size-6 items-center justify-center rounded bg-background/70 text-foreground opacity-0 group-hover:opacity-100 hover:bg-background"
        >
          <Maximize2 className="size-3.5" />
        </button>
      </div>
      <div className="flex min-h-0 flex-1 flex-col gap-1 p-2">
        <div className="flex items-start gap-1">
          <Tooltip>
            <TooltipTrigger asChild>
              <div className="min-w-0 flex-1 truncate text-xs font-medium text-foreground">
                {result.name}
              </div>
            </TooltipTrigger>
            <TooltipContent>{result.name}</TooltipContent>
          </Tooltip>
          <button
            type="button"
            aria-label="Open on the provider's site"
            className="shrink-0 rounded-sm p-0.5 text-muted-foreground hover:bg-accent hover:text-foreground"
            onClick={() => {
              // WebKitGTK ignores window.open / <a target> to external URLs — go through the bridge.
              void invoke("open_external", { url: result.sourceUrl }).catch((err: unknown) =>
                notifyError(errorText(err)),
              );
            }}
          >
            <ExternalLink className="size-3.5" />
          </button>
        </div>
        <div className="flex items-center gap-1">
          {result.author ? (
            <span className="truncate text-[10px] text-muted-foreground">{result.author}</span>
          ) : null}
          <Badge variant="outline" className="ml-auto text-[10px] uppercase">
            {result.license.id}
          </Badge>
        </div>
        <div className="mt-auto">
          {/* The heavy Radix controls mount only on hover, so a fast-scroll re-mount of many
              cards stays cheap and the grid doesn't blank. */}
          <ImportControls result={result} interactive={hovered} />
        </div>
      </div>
      {expanded ? (
        <AssetDetailModal
          result={result}
          images={images}
          open={expanded && active}
          onOpenChange={setExpanded}
        />
      ) : null}
    </div>
  );
});
