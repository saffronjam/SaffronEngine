// The Store results grid: a windowed grid of result cards fed by infinite scroll. Each
// source advances its own cursor server-side; we just pull the next round-robin batch as
// the user nears the end, and stop when the session reports all sources exhausted.
import * as React from "react";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { ExternalLink, Loader2, Maximize2 } from "lucide-react";

import { Badge } from "@/components/ui/badge";
import { Tooltip, TooltipContent, TooltipTrigger } from "@/components/ui/tooltip";

import { errorText, notifyError } from "../lib/flash";
import { AssetDetailModal } from "./AssetDetailModal";
import { GalleryViewer } from "./GalleryViewer";
import { ImportControls } from "./ImportControls";
import { storeSearchMore, type StoreResult } from "./types";
import { useGallery, useGalleryNav } from "./useGallery";

const CELL_W = 196; // px — tile + gap
const CELL_H = 232;
const OVERSCAN_ROWS = 2;
const BATCH = 24;

export function StoreResultsGrid({
  session,
  active,
  onLoadingChange,
}: {
  session: string;
  active: boolean;
  onLoadingChange?: (loading: boolean) => void;
}) {
  const [results, setResults] = useState<StoreResult[]>([]);
  const [exhausted, setExhausted] = useState(false);
  const [loading, setLoading] = useState(false);

  const scrollRef = useRef<HTMLDivElement | null>(null);
  const [scrollTop, setScrollTop] = useState(0);
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
        setResults((prev) => (reset ? page.results : [...prev, ...page.results]));
        setExhausted(page.exhausted);
      })
      .catch((err: unknown) => {
        setExhausted(true);
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

  // A fresh session reloads from the top; results are kept until the first batch lands.
  useEffect(() => {
    setExhausted(false);
    if (scrollRef.current) scrollRef.current.scrollTop = 0;
    setScrollTop(0);
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
    const ro = new ResizeObserver(() => setViewport({ w: el.clientWidth, h: el.clientHeight }));
    ro.observe(el);
    setViewport({ w: el.clientWidth, h: el.clientHeight });
    return () => ro.disconnect();
  }, []);

  const columns = Math.max(1, Math.floor((viewport.w || CELL_W) / CELL_W));
  const rows = Math.ceil(results.length / columns);
  const totalHeight = rows * CELL_H;

  const onScroll = (e: React.UIEvent<HTMLDivElement>) => {
    const el = e.currentTarget;
    setScrollTop(el.scrollTop);
    if (el.scrollHeight - el.clientHeight - el.scrollTop < CELL_H * 3) {
      loadMore();
    }
  };

  const startRow = Math.max(0, Math.floor(scrollTop / CELL_H) - OVERSCAN_ROWS);
  const endRow = Math.min(rows, Math.ceil((scrollTop + viewport.h) / CELL_H) + OVERSCAN_ROWS);
  const visible = useMemo(() => {
    const out: { result: StoreResult; index: number }[] = [];
    for (let i = startRow * columns; i < Math.min(results.length, endRow * columns); i++) {
      out.push({ result: results[i], index: i });
    }
    return out;
  }, [results, startRow, endRow, columns]);

  return (
    <div className="relative min-h-0 flex-1">
      <div ref={scrollRef} onScroll={onScroll} className="absolute inset-0 overflow-auto p-2">
        {results.length === 0 && !loading ? (
          <p className="p-8 text-center text-sm text-muted-foreground italic">
            Nothing here — try a different search.
          </p>
        ) : (
          <div style={{ height: totalHeight, position: "relative" }}>
            {visible.map(({ result, index }) => (
              <StoreCard
                key={`${result.store.id}:${result.id}`}
                result={result}
                active={active}
                top={Math.floor(index / columns) * CELL_H}
                left={(index % columns) * CELL_W}
              />
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
  top,
  left,
}: {
  result: StoreResult;
  active: boolean;
  top: number;
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
    <div
      className="group absolute flex flex-col overflow-hidden rounded-md border border-border bg-card"
      style={{ top, left, width: CELL_W - 12, height: CELL_H - 12 }}
      onMouseEnter={() => setHovered(true)}
    >
      <div className="relative h-28 w-full shrink-0 bg-muted">
        <GalleryViewer images={images} nav={nav} alt={result.name} />
        <Badge variant="secondary" className="absolute top-1 left-1 text-[10px]">
          {result.store.displayName}
        </Badge>
        <button
          type="button"
          aria-label="Expand"
          onClick={() => setExpanded(true)}
          className="absolute top-1 right-1 flex size-6 items-center justify-center rounded bg-background/70 text-foreground opacity-0 transition-opacity group-hover:opacity-100 hover:bg-background"
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
          <ImportControls result={result} />
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
