// The Store main tab: browse one store at a time. A store dropdown (top-left) picks which of the
// project's enabled connectors to search; the centered search bar queries that store (Enter /
// chip-commit only). A gear opens the provider-setup modal and an icon toggles the credits view.
//
// Which connectors a project uses persists in project.json's `stores` block (shared with the
// team); each connector's secret lives only in the OS keyring (per machine). The selected store +
// last query persist app-wide (localStorage) so reopening the Store returns you to where you left
// off. Opening the Store with nothing enabled auto-opens the provider modal ("Add your first
// provider"); disabling the store you're viewing auto-selects another enabled one.
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { Award, Loader2, Settings } from "lucide-react";

import { Button } from "@/components/ui/button";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { Tooltip, TooltipContent, TooltipTrigger } from "@/components/ui/tooltip";

import { AnimaSearchbar } from "../components/anima/AnimaSearchbar";
import type { ChipConfig, SearchState } from "../components/anima/chipSearch";
import { client } from "../control/client";
import { errorText, notifyError } from "../lib/flash";
import { useEditorStore } from "../state/store";
import { ProviderLogo } from "./connectorIcons";
import { ProviderModal } from "./ProviderModal";
import { StoreCredits } from "./StoreCredits";
import { StoreOverlayProvider } from "./storeOverlay";
import { StoreResultsGrid } from "./StoreResultsGrid";
import {
  storeListConnectors,
  storeSearchSession,
  type ConnectorInfo,
  type SearchQuery,
  type StoreKind,
} from "./types";

const KIND_OPTIONS: { value: StoreKind; label: string }[] = [
  { value: "model", label: "Model" },
  { value: "hdri", label: "HDRI" },
  { value: "material", label: "Material" },
  { value: "texture", label: "Texture" },
];

// AnimaSearchbar emits onChange on a debounce for free-text typing and immediately on a commit
// (Enter / chip). A very large debounce suppresses the typing emit, so onChange reaches us only
// on a committed search — the "search on Enter only" rule.
const COMMIT_ONLY_DEBOUNCE_MS = 600_000;

/// Rebuild the searchbar state from a persisted query (free text + optional kind chip).
function searchStateFor(text: string, kind: StoreKind | null): SearchState {
  return { chips: kind ? [{ keyword: "type", value: kind }] : [], freeText: text };
}

// `active` is false while the tab is mounted-but-hidden; it drives the auto-focus and landing-search
// effects (the Store dialogs stay open across tab switches — they're hidden with the view, which is
// `display:none` when inactive).
export function StoreWorkspace({ active }: { active: boolean }) {
  const [connectors, setConnectors] = useState<ConnectorInfo[]>([]);
  const [enabled, setEnabled] = useState<string[]>([]);
  const [loaded, setLoaded] = useState(false);
  const [providerModalOpen, setProviderModalOpen] = useState(false);
  // The Store view's own region: the scoped dialogs (detail / providers) portal into it so they
  // dim only this area and leave the main tab strip live, rather than covering the whole window.
  const [overlayHost, setOverlayHost] = useState<HTMLElement | null>(null);
  const [showCredits, setShowCredits] = useState(false);
  // Seed the searchbar from the persisted query so a reopen shows where you left off.
  const [search, setSearch] = useState<SearchState>(() => {
    const s = useEditorStore.getState();
    return searchStateFor(s.storeSearchText, s.storeKind);
  });
  const [searching, setSearching] = useState(false);
  const searchRef = useRef<HTMLInputElement | null>(null);

  const storeSelected = useEditorStore((s) => s.storeSelected);
  const storeSession = useEditorStore((s) => s.storeSession);

  // Auto-focus the searchbar when the tab opens or is returned to (but not behind the
  // provider modal, which owns focus). rAF so the just-unhidden input is focusable.
  useEffect(() => {
    if (!active || !loaded || providerModalOpen) return;
    const id = requestAnimationFrame(() => searchRef.current?.focus());
    return () => cancelAnimationFrame(id);
  }, [active, loaded, providerModalOpen]);

  const runSearch = useCallback((next: SearchState) => {
    const store = useEditorStore.getState();
    setSearch(next);
    setShowCredits(false);
    const kindChip = next.chips.find((c) => c.keyword === "type");
    const kind = kindChip ? (kindChip.value as StoreKind) : null;
    const text = next.freeText.trim();
    store.setStoreQuery({ text, kind });
    const provider = store.storeSelected;
    if (!provider) return;
    const query: SearchQuery = { text, kind: kind ?? undefined, provider };
    storeSearchSession(query)
      .then((id) => useEditorStore.getState().setStoreSession(id))
      .catch((err: unknown) => notifyError(errorText(err)));
  }, []);

  useEffect(() => {
    Promise.all([storeListConnectors(), client.getStores()])
      .then(([list, stores]) => {
        setConnectors(list);
        const enabledList = stores.enabled ?? [];
        setEnabled(enabledList);
        // First use: nothing enabled → prompt to add a provider.
        if (enabledList.length === 0) {
          setProviderModalOpen(true);
          return;
        }
        // Reconcile the persisted store against what the project actually enables; if it's gone,
        // fall to the first enabled store and drop any stale session so a fresh search runs.
        const store = useEditorStore.getState();
        if (store.storeSelected == null || !enabledList.includes(store.storeSelected)) {
          store.setStoreSelected(enabledList[0] ?? null);
          store.resetStoreBrowse();
        }
      })
      .catch((err: unknown) => notifyError(errorText(err)))
      .finally(() => setLoaded(true));
  }, []);

  const persistEnabled = useCallback(
    (next: string[]) => {
      setEnabled(next);
      // Auto-select if the store being viewed was just disabled (or nothing was selected yet).
      const store = useEditorStore.getState();
      if (store.storeSelected == null || !next.includes(store.storeSelected)) {
        const fallback = next[0] ?? null;
        store.setStoreSelected(fallback);
        if (fallback) runSearch(search);
        else store.resetStoreBrowse();
      }
      client
        .setStores(next)
        .then(() => client.saveProject())
        .catch((err: unknown) => notifyError(errorText(err)));
    },
    [runSearch, search],
  );

  const chips = useMemo<ChipConfig[]>(
    () => [
      {
        keyword: "type",
        label: "Type",
        options: (input) => {
          const needle = input.toLowerCase();
          return KIND_OPTIONS.filter((o) => o.value.includes(needle)).map((o) => ({
            value: o.value,
            label: o.label,
          }));
        },
        resolveLabel: (value) => KIND_OPTIONS.find((o) => o.value === value)?.label ?? null,
      },
    ],
    [],
  );

  const enabledConnectors = useMemo(
    () => connectors.filter((c) => enabled.includes(c.id)),
    [connectors, enabled],
  );
  const selectedConnector = enabledConnectors.find((c) => c.id === storeSelected) ?? null;

  const selectStore = (id: string) => {
    useEditorStore.getState().setStoreSelected(id);
    // Re-run the current search against the newly-selected store.
    runSearch(search);
  };

  // Landing / restore: with a store selected but no live session (first open, or after a restart
  // that cleared the in-memory session), run the restored query so the grid is prefilled. Runs
  // once — `storeSession` is set afterwards, so the guard stops it repeating.
  useEffect(() => {
    if (active && loaded && storeSelected != null && storeSession === null) {
      runSearch(search);
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [active, loaded, storeSelected, storeSession]);

  if (!loaded) {
    return (
      <main className="flex min-h-0 flex-1 items-center justify-center bg-background">
        <Loader2 className="size-8 animate-spin text-muted-foreground" />
      </main>
    );
  }

  return (
    <main ref={setOverlayHost} className="relative flex min-h-0 flex-1 flex-col bg-background">
      <StoreOverlayProvider value={overlayHost}>
        <div className="relative flex shrink-0 items-center border-b border-border p-2">
          <div className="absolute inset-y-0 left-2 flex items-center">
            <Select value={storeSelected ?? ""} onValueChange={selectStore} perfLabel="provider">
              <SelectTrigger size="sm" className="w-48 gap-2" aria-label="Store">
                {selectedConnector ? (
                  <span className="flex min-w-0 items-center gap-2">
                    <ProviderLogo
                      connectorId={selectedConnector.id}
                      name={selectedConnector.displayName}
                      className="size-4 shrink-0"
                    />
                    <span className="truncate">{selectedConnector.displayName}</span>
                  </span>
                ) : (
                  <SelectValue placeholder="Select store" />
                )}
              </SelectTrigger>
              <SelectContent align="start">
                {enabledConnectors.map((c) => (
                  <SelectItem key={c.id} value={c.id}>
                    <span className="flex items-center gap-2">
                      <ProviderLogo
                        connectorId={c.id}
                        name={c.displayName}
                        className="size-4 shrink-0"
                      />
                      {c.displayName}
                    </span>
                  </SelectItem>
                ))}
              </SelectContent>
            </Select>
          </div>
          <div className="mx-auto w-[60%]">
            <AnimaSearchbar
              value={search}
              onChange={runSearch}
              chips={chips}
              placeholder="Search assets — press Enter"
              debounceMs={COMMIT_ONLY_DEBOUNCE_MS}
              inputRef={searchRef}
              busy={searching}
            />
          </div>
          <div className="absolute top-1/2 right-2 flex -translate-y-1/2 items-center gap-1">
            <Tooltip>
              <TooltipTrigger asChild>
                <Button
                  type="button"
                  size="icon-sm"
                  variant={showCredits ? "secondary" : "ghost"}
                  onClick={() => setShowCredits((c) => !c)}
                  aria-label="Credits"
                >
                  <Award />
                </Button>
              </TooltipTrigger>
              <TooltipContent>Asset credits</TooltipContent>
            </Tooltip>
            <Tooltip>
              <TooltipTrigger asChild>
                <Button
                  type="button"
                  size="icon-sm"
                  variant="ghost"
                  onClick={() => setProviderModalOpen(true)}
                  aria-label="Manage providers"
                >
                  <Settings />
                </Button>
              </TooltipTrigger>
              <TooltipContent>Manage providers</TooltipContent>
            </Tooltip>
          </div>
        </div>

        {showCredits ? (
          <StoreCredits />
        ) : storeSession === null ? (
          <div className="flex flex-1 items-center justify-center p-6 text-center text-sm text-muted-foreground italic">
            Nothing here — search for any asset.
          </div>
        ) : (
          <StoreResultsGrid session={storeSession} onLoadingChange={setSearching} />
        )}

        <ProviderModal
          open={providerModalOpen}
          onOpenChange={setProviderModalOpen}
          connectors={connectors}
          enabled={enabled}
          onEnabledChange={persistEnabled}
        />
      </StoreOverlayProvider>
    </main>
  );
}
