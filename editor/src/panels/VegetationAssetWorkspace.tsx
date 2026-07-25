/// Read-only catalog view for the three authored vegetation asset domains.
import { useEffect, useMemo, useState, type ReactNode } from "react";
import {
  CircleCheck,
  CircleX,
  Gauge,
  GitBranch,
  Info,
  Layers3,
  Map as MapIcon,
  Sprout,
  TreePine,
  type LucideIcon,
} from "lucide-react";
import { client } from "../control/client";
import { useEditorStore, type VegetationAssetType } from "../state/store";
import type { CommandResultMap } from "../protocol";
import { errorText, notifyError } from "../lib/flash";
import { humanizeFieldName } from "../lib/humanize";
import { Badge } from "@/components/ui/badge";
import { ScrollArea } from "@/components/ui/scroll-area";
import {
  describeVegetationDependency,
  formatVegetationBytes,
  formatVegetationCacheRate,
  formatVegetationCount,
  formatVegetationDuration,
} from "./vegetationAssetDetails";

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
                ids={summary.summary.asset.biomeInstances.map((row) => row.biome)}
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

          <ValidationSection validation={subject.validation} />
          <ProvenanceSection provenance={subject.provenance} />
          <DependenciesSection dependencies={subject.dependencies} assetNames={assetNames} />
          <CookStatisticsSection statistics={subject.latestCook} />

          <p className="font-mono text-[10px] text-muted-foreground">Asset ID {subject.id}</p>
        </main>
      </ScrollArea>
    </div>
  );
}

function ValidationSection({
  validation,
}: {
  validation: VegetationSummary["summary"]["asset"]["validation"];
}) {
  return (
    <section className="overflow-hidden rounded-md border border-border bg-card">
      <SectionHeader
        icon={validation.valid ? CircleCheck : CircleX}
        title="Validation"
        trailing={
          <Badge variant={validation.valid ? "default" : "destructive"}>
            {validation.valid ? "Valid" : "Invalid"}
          </Badge>
        }
      />
      {validation.issues.length === 0 ? (
        <EmptySection>No validation issues</EmptySection>
      ) : (
        <div className="divide-y divide-border">
          {validation.issues.map((issue) => (
            <div
              key={`${issue.severity}:${issue.code}:${issue.path}:${issue.sourceSelector ?? ""}:${issue.message}`}
              className="grid grid-cols-[auto_minmax(0,1fr)] gap-3 px-4 py-3"
            >
              <ValidationBadge severity={issue.severity} />
              <div className="min-w-0">
                <div className="flex flex-wrap items-baseline gap-x-2 gap-y-1">
                  <span className="text-sm font-medium">{issue.message}</span>
                  <code className="text-[10px] text-muted-foreground">{issue.code}</code>
                </div>
                <p className="mt-1 break-all font-mono text-[10px] text-muted-foreground">
                  {issue.path}
                </p>
                {issue.sourceSelector === undefined ? null : (
                  <p className="mt-1 break-all text-xs text-muted-foreground">
                    Source selector: {issue.sourceSelector}
                  </p>
                )}
              </div>
            </div>
          ))}
        </div>
      )}
    </section>
  );
}

function ValidationBadge({
  severity,
}: {
  severity: VegetationSummary["summary"]["asset"]["validation"]["issues"][number]["severity"];
}) {
  if (severity === "error") {
    return <Badge variant="destructive">Error</Badge>;
  }
  if (severity === "warning") {
    return (
      <Badge variant="outline" className="border-amber-500/50 text-amber-500">
        Warning
      </Badge>
    );
  }
  return <Badge variant="secondary">Info</Badge>;
}

function ProvenanceSection({
  provenance,
}: {
  provenance: VegetationSummary["summary"]["asset"]["provenance"];
}) {
  return (
    <section className="overflow-hidden rounded-md border border-border bg-card">
      <SectionHeader icon={Info} title="Provenance" count={provenance.length} />
      {provenance.length === 0 ? (
        <EmptySection>No source provenance recorded</EmptySection>
      ) : (
        <div className="divide-y divide-border">
          {provenance.map((entry) => (
            <div
              key={`${entry.source}:${entry.sourceUri}:${entry.licenseId}:${entry.author}:${entry.attribution}`}
              className="px-4 py-3"
            >
              <div className="flex flex-wrap items-center gap-2">
                <span className="text-sm font-medium">{entry.source || "Unnamed source"}</span>
                {entry.requiresAttribution ? (
                  <Badge variant="outline">Attribution required</Badge>
                ) : null}
              </div>
              <MetadataGrid>
                <Metadata label="Source URI" value={entry.sourceUri || "Not declared"} />
                <Metadata label="Author" value={entry.author || "Not declared"} />
                <Metadata label="License" value={entry.licenseId || "Not declared"} />
                <Metadata label="License URI" value={entry.licenseUri || "Not declared"} />
              </MetadataGrid>
              {entry.attribution ? (
                <p className="mt-3 whitespace-pre-wrap text-xs text-muted-foreground">
                  {entry.attribution}
                </p>
              ) : null}
            </div>
          ))}
        </div>
      )}
    </section>
  );
}

function DependenciesSection({
  dependencies,
  assetNames,
}: {
  dependencies: VegetationSummary["summary"]["asset"]["dependencies"];
  assetNames: ReadonlyMap<string, string>;
}) {
  return (
    <section className="overflow-hidden rounded-md border border-border bg-card">
      <SectionHeader icon={GitBranch} title="Dependencies" count={dependencies.length} />
      {dependencies.length === 0 ? (
        <EmptySection>No dependencies</EmptySection>
      ) : (
        <div className="divide-y divide-border">
          {dependencies.map((dependency) => {
            const description = describeVegetationDependency(dependency.address, assetNames);
            return (
              <div
                key={`${dependency.contentHash}:${JSON.stringify(dependency.address)}`}
                className="grid gap-3 px-4 py-3 md:grid-cols-[minmax(0,1fr)_minmax(12rem,0.55fr)]"
              >
                <div className="min-w-0">
                  <Badge variant="secondary">{description.kind}</Badge>
                  <p className="mt-2 break-all text-xs">{description.subject}</p>
                  {dependency.bounds === undefined ? null : (
                    <p className="mt-1 break-all font-mono text-[10px] text-muted-foreground">
                      Bounds {dependency.bounds.minTicks.join(", ")} →{" "}
                      {dependency.bounds.maxTicksExclusive.join(", ")}
                    </p>
                  )}
                </div>
                <div className="min-w-0 text-xs text-muted-foreground md:text-right">
                  <p className="break-all font-mono text-[10px]">{dependency.contentHash}</p>
                  <p className="mt-1">
                    Halo bits {dependency.haloBits}
                    {dependency.ancestorLevel === undefined
                      ? ""
                      : ` · ancestor level ${dependency.ancestorLevel}`}
                  </p>
                </div>
              </div>
            );
          })}
        </div>
      )}
    </section>
  );
}

function CookStatisticsSection({
  statistics,
}: {
  statistics: VegetationSummary["summary"]["asset"]["latestCook"];
}) {
  return (
    <section className="overflow-hidden rounded-md border border-border bg-card">
      <SectionHeader icon={Gauge} title="Latest cook" />
      {statistics === undefined ? (
        <EmptySection>No cook statistics recorded</EmptySection>
      ) : (
        <>
          <div className="grid gap-px border-b border-border bg-border sm:grid-cols-2 lg:grid-cols-4">
            <CookFact label="Nodes" value={formatVegetationCount(statistics.nodes)} />
            <CookFact label="Elapsed" value={formatVegetationDuration(statistics.elapsedMicros)} />
            <CookFact
              label="Peak memory"
              value={formatVegetationBytes(statistics.peakMemoryBytes)}
            />
            <CookFact
              label="Cache hit rate"
              value={formatVegetationCacheRate(statistics.cacheHits, statistics.cacheMisses)}
            />
            <CookFact label="Input" value={formatVegetationBytes(statistics.inputBytes)} />
            <CookFact label="Output" value={formatVegetationBytes(statistics.outputBytes)} />
            <CookFact
              label="Cache hits / misses"
              value={`${formatVegetationCount(statistics.cacheHits)} / ${formatVegetationCount(statistics.cacheMisses)}`}
            />
            <CookFact
              label="Published cells"
              value={formatVegetationCount(statistics.publishedCells)}
            />
          </div>
          <div className="px-4 py-3">
            <h3 className="text-xs font-medium">Rejections</h3>
            {statistics.rejections.length === 0 ? (
              <p className="mt-2 text-xs text-muted-foreground">No candidates rejected</p>
            ) : (
              <div className="mt-2 flex flex-wrap gap-2">
                {statistics.rejections.map((rejection) => (
                  <Badge key={rejection.reason} variant="outline">
                    {humanizeFieldName(rejection.reason)} · {formatVegetationCount(rejection.count)}
                  </Badge>
                ))}
              </div>
            )}
          </div>
        </>
      )}
    </section>
  );
}

function SectionHeader({
  icon: Icon,
  title,
  count,
  trailing,
}: {
  icon: LucideIcon;
  title: string;
  count?: number;
  trailing?: ReactNode;
}) {
  return (
    <div className="flex items-center gap-2 border-b border-border px-4 py-3">
      <Icon className="size-4 text-muted-foreground" />
      <h2 className="text-sm font-medium">{title}</h2>
      {count === undefined ? null : <Badge variant="secondary">{count}</Badge>}
      {trailing === undefined ? null : <div className="ml-auto">{trailing}</div>}
    </div>
  );
}

function EmptySection({ children }: { children: ReactNode }) {
  return <p className="px-4 py-5 text-xs text-muted-foreground">{children}</p>;
}

function MetadataGrid({ children }: { children: ReactNode }) {
  return <div className="mt-3 grid gap-3 sm:grid-cols-2">{children}</div>;
}

function Metadata({ label, value }: { label: string; value: string }) {
  return (
    <div className="min-w-0">
      <p className="text-[10px] uppercase tracking-wide text-muted-foreground">{label}</p>
      <p className="mt-0.5 break-all text-xs">{value}</p>
    </div>
  );
}

function CookFact({ label, value }: { label: string; value: string }) {
  return (
    <div className="min-w-0 bg-card px-4 py-3">
      <p className="text-[10px] uppercase tracking-wide text-muted-foreground">{label}</p>
      <p className="mt-1 break-words font-mono text-sm font-medium">{value}</p>
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
