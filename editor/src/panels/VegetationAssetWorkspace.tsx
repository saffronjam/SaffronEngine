/// Read-only catalog view for the three authored vegetation asset domains.
import { useEffect, useMemo, useState, type ReactNode } from "react";
import { Layers3, Map as MapIcon, Sprout, TreePine, type LucideIcon } from "lucide-react";
import { client } from "../control/client";
import { useEditorStore, type VegetationAssetType } from "../state/store";
import type { CommandResultMap } from "../protocol";
import { errorText, notifyError } from "../lib/flash";
import { humanizeFieldName } from "../lib/humanize";
import { Badge } from "@/components/ui/badge";
import { ScrollArea } from "@/components/ui/scroll-area";

type VegetationSummary = CommandResultMap["vegetation-asset-summary"];
type LoadState = "loading" | "ready" | "unavailable";

interface VegetationAssetWorkspaceProps {
  assetId: string;
  assetType: VegetationAssetType;
}

export function VegetationAssetWorkspace({ assetId, assetType }: VegetationAssetWorkspaceProps) {
  const assets = useEditorStore((state) => state.assets);
  const [summary, setSummary] = useState<VegetationSummary | null>(null);
  const [loadState, setLoadState] = useState<LoadState>("loading");
  const assetNames = useMemo(
    () => new Map(assets.map((asset) => [asset.id, asset.name] as const)),
    [assets],
  );

  useEffect(() => {
    let cancelled = false;
    setSummary(null);
    setLoadState("loading");
    void client
      .vegetationAssetSummary(assetId)
      .then((result) => {
        if (!cancelled) {
          setSummary(result);
          setLoadState("ready");
        }
      })
      .catch((err) => {
        if (!cancelled) {
          setLoadState("unavailable");
          notifyError(errorText(err));
        }
      });
    return () => {
      cancelled = true;
    };
  }, [assetId]);

  if (loadState !== "ready" || summary === null) {
    return (
      <div className="contain-panel flex min-h-0 min-w-0 flex-1 items-center justify-center bg-background text-sm text-muted-foreground">
        {loadState === "loading" ? "Loading vegetation asset…" : "Summary unavailable"}
      </div>
    );
  }

  const expectedKind = assetType;
  if (summary.summary.kind !== expectedKind) {
    return (
      <div className="contain-panel flex min-h-0 min-w-0 flex-1 items-center justify-center bg-background text-sm text-muted-foreground">
        Asset type changed; reopen it from the catalog.
      </div>
    );
  }

  const Icon = iconFor(assetType);
  const subject = summary.summary.asset;

  return (
    <div className="contain-panel flex min-h-0 min-w-0 flex-1 flex-col bg-background text-foreground">
      <header className="flex flex-none items-center gap-3 border-b border-border px-5 py-4">
        <span className="flex size-10 items-center justify-center rounded-md bg-muted text-muted-foreground">
          <Icon className="size-5" />
        </span>
        <div className="min-w-0 flex-1">
          <h1 className="truncate text-base font-semibold">{subject.name}</h1>
          <p className="text-xs text-muted-foreground">{labelFor(assetType)}</p>
        </div>
        <Badge variant="secondary">Schema v{subject.version}</Badge>
      </header>

      <ScrollArea className="min-h-0 flex-1">
        <main className="mx-auto flex w-full max-w-5xl flex-col gap-5 p-5">
          {summary.summary.kind === "plant" ? (
            <SummaryGrid>
              <Fact label="Source" value={humanizeFieldName(summary.summary.asset.source)} />
              <Fact label="Semantic parts" value={summary.summary.asset.partCount} />
              <Fact label="Phenotypes" value={summary.summary.asset.phenotypeCount} />
              <Fact label="Material slots" value={summary.summary.asset.materialSlots.length} />
            </SummaryGrid>
          ) : null}

          {summary.summary.kind === "biome" ? (
            <SummaryGrid>
              <Fact label="Role" value={humanizeFieldName(summary.summary.asset.role)} />
              <Fact label="Plant palette" value={summary.summary.asset.plantPalette.length} />
              <Fact label="Modules" value={summary.summary.asset.modules.length} />
              <Fact label="Parameters" value={summary.summary.asset.parameterCount} />
            </SummaryGrid>
          ) : null}

          {summary.summary.kind === "vegetation-map" ? (
            <SummaryGrid>
              <Fact label="Layers" value={summary.summary.asset.layerCount} />
              <Fact label="Biome instances" value={summary.summary.asset.biomeInstances.length} />
              <Fact label="Chunk level" value={summary.summary.asset.chunkLevel} />
              <Fact
                label="World bounds"
                value={`${summary.summary.asset.bounds.minTicks.join(", ")} → ${summary.summary.asset.bounds.maxTicksExclusive.join(", ")}`}
              />
            </SummaryGrid>
          ) : null}

          {summary.summary.kind === "plant" ? (
            <ReferenceList
              title="Material slots"
              ids={summary.summary.asset.materialSlots}
              assetNames={assetNames}
            />
          ) : null}
          {summary.summary.kind === "biome" ? (
            <>
              <ReferenceList
                title="Plant palette"
                ids={summary.summary.asset.plantPalette}
                assetNames={assetNames}
              />
              <ReferenceList
                title="Graph modules"
                ids={summary.summary.asset.modules}
                assetNames={assetNames}
              />
            </>
          ) : null}
          {summary.summary.kind === "vegetation-map" ? (
            <>
              <ReferenceList
                title="Biome instances"
                ids={summary.summary.asset.biomeInstances}
                assetNames={assetNames}
              />
              <section className="overflow-hidden rounded-md border border-border bg-card">
                <div className="flex items-center gap-2 border-b border-border px-4 py-3">
                  <Layers3 className="size-4 text-muted-foreground" />
                  <h2 className="text-sm font-medium">Ordered layers</h2>
                </div>
                {summary.layers.length === 0 ? (
                  <p className="px-4 py-5 text-xs text-muted-foreground">No authored layers</p>
                ) : (
                  <div className="divide-y divide-border">
                    {summary.layers.map((layer) => (
                      <div
                        key={layer.id}
                        className="grid grid-cols-[minmax(0,1fr)_auto] gap-4 px-4 py-3"
                      >
                        <div className="min-w-0">
                          <div className="flex items-center gap-2">
                            <span className="truncate text-sm font-medium">{layer.name}</span>
                            {layer.locked ? <Badge variant="outline">Locked</Badge> : null}
                            {layer.muted ? <Badge variant="outline">Muted</Badge> : null}
                          </div>
                          <p className="mt-1 truncate font-mono text-[10px] text-muted-foreground">
                            {layer.id}
                          </p>
                          <p className="mt-1 text-xs text-muted-foreground">
                            {layer.dependencies.length} dependencies · revision {layer.revision}
                          </p>
                        </div>
                        <div className="flex flex-col items-end gap-1 text-xs text-muted-foreground">
                          <Badge variant="secondary">
                            {humanizeFieldName(layer.operator.kind)}
                          </Badge>
                          <span>{humanizeFieldName(layer.coordinateSpace)}</span>
                          <span>Order {layer.order}</span>
                        </div>
                      </div>
                    ))}
                  </div>
                )}
              </section>
            </>
          ) : null}

          <p className="font-mono text-[10px] text-muted-foreground">Asset ID {subject.id}</p>
        </main>
      </ScrollArea>
    </div>
  );
}

function SummaryGrid({ children }: { children: ReactNode }) {
  return <section className="grid gap-3 sm:grid-cols-2 lg:grid-cols-4">{children}</section>;
}

function Fact({ label, value }: { label: string; value: ReactNode }) {
  return (
    <div className="min-w-0 rounded-md border border-border bg-card px-4 py-3">
      <p className="text-[10px] uppercase tracking-wide text-muted-foreground">{label}</p>
      <p className="mt-1 break-words text-sm font-medium">{value}</p>
    </div>
  );
}

function ReferenceList({
  title,
  ids,
  assetNames,
}: {
  title: string;
  ids: string[];
  assetNames: ReadonlyMap<string, string>;
}) {
  return (
    <section className="overflow-hidden rounded-md border border-border bg-card">
      <h2 className="border-b border-border px-4 py-3 text-sm font-medium">{title}</h2>
      {ids.length === 0 ? (
        <p className="px-4 py-5 text-xs text-muted-foreground">None</p>
      ) : (
        <div className="divide-y divide-border">
          {ids.map((id) => (
            <div key={id} className="flex items-center justify-between gap-4 px-4 py-2.5">
              <span className="truncate text-xs">{assetNames.get(id) ?? "Unresolved asset"}</span>
              <code className="truncate text-[10px] text-muted-foreground">{id}</code>
            </div>
          ))}
        </div>
      )}
    </section>
  );
}

function iconFor(type: VegetationAssetType): LucideIcon {
  if (type === "plant") {
    return Sprout;
  }
  if (type === "biome") {
    return TreePine;
  }
  return MapIcon;
}

function labelFor(type: VegetationAssetType): string {
  if (type === "plant") {
    return "Plant family asset";
  }
  if (type === "biome") {
    return "Biome graph asset";
  }
  return "Vegetation map asset";
}
