/// The Vegetation mode dock panel: the authoring tool palette (select/paint/erase/…),
/// the brush parameters the viewport tools read, and the live render population from
/// `vegetation-render-stats` (per-family instances/tiles/predicted plus page faults).
/// It is a closable "editing" panel outside the default layout — Vegetation is an
/// on-demand world-building mode, not a resident panel.
import { useEffect, useRef, useState } from "react";
import { ChevronDown, ChevronUp, Headphones, Lock, LockOpen, Volume2, VolumeX } from "lucide-react";
import { client } from "../control/client";
import { isBusyLoading } from "../control/client";
import { errorText, notifyError } from "../lib/flash";
import type {
  VegetationCookStatusDto,
  VegetationEvaluationPreflightDto,
  VegetationFamilyRenderDto,
  VegetationLayerDto,
  VegetationRenderStatsDto,
  VegetationTopologyDiffResult,
  WorldBoundsDto,
} from "../protocol";
import { SliderField } from "../components/SliderField";
import {
  getCachedThumbnailUrl,
  getThumbnailUrl,
  useEditorStore,
  type VegetationPaintTarget,
} from "../state/store";
import {
  VEGETATION_TOOLS,
  isBrushTool,
  isRuntimeTool,
  isStrokeTool,
  usesFalloff,
  usesTargetDensity,
} from "./vegetationTools";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { ScrollArea } from "@/components/ui/scroll-area";
import { Separator } from "@/components/ui/separator";
import { Tooltip, TooltipContent, TooltipTrigger } from "@/components/ui/tooltip";
import { SectionLabel } from "../components/PanelRows";

function BrushField({ label, children }: { label: string; children: React.ReactNode }) {
  return (
    <div className="flex items-center gap-2">
      <span className="w-20 shrink-0 text-[11px] text-muted-foreground">{label}</span>
      <div className="min-w-0 flex-1">{children}</div>
    </div>
  );
}

function Stat({ label, value }: { label: string; value: React.ReactNode }) {
  return (
    <div className="flex items-baseline justify-between gap-2">
      <span className="text-[11px] text-muted-foreground">{label}</span>
      <span className="font-mono text-[11px] tabular-nums text-foreground">{value}</span>
    </div>
  );
}

function shortFamily(id: string): string {
  return id.length > 10 ? `…${id.slice(-8)}` : id;
}

/// A species row's rendered-family swatch (the shared thumbnail cache; blank until
/// the engine's tile lands, blank on a failed generation).
function SpeciesThumb({ id, name }: { id: string; name: string }) {
  const [url, setUrl] = useState<string | null>(() => getCachedThumbnailUrl(id, 64));
  useEffect(() => {
    if (url !== null) {
      return;
    }
    let live = true;
    getThumbnailUrl(id, 64)
      .then((next) => {
        if (live) {
          setUrl(next);
        }
      })
      .catch(() => {});
    return () => {
      live = false;
    };
  }, [id, url]);
  return (
    <span className="flex size-6 shrink-0 items-center justify-center overflow-hidden rounded bg-muted">
      {url !== null && (
        <img src={url} alt={name} className="size-full object-contain" draggable={false} />
      )}
    </span>
  );
}

/// The bound vegetation map and its authored layers, polled beside the render
/// stats: the runtime status names the map, and the asset summary lists the map's
/// ordered layers plus its dirty set (refetched every tick while the panel is
/// open, so stroke commits and recooks keep the badges honest).
function useVegetationMap(): {
  map: string | null;
  generation: string | null;
  chunkLevel: number | null;
  dirtyLayers: ReadonlySet<string>;
  bounds: WorldBoundsDto | null;
  biomeInstance: string | null;
  layers: VegetationLayerDto[];
  refresh: () => void;
} {
  const [map, setMap] = useState<string | null>(null);
  const [generation, setGeneration] = useState<string | null>(null);
  const [chunkLevel, setChunkLevel] = useState<number | null>(null);
  const [dirtyLayers, setDirtyLayers] = useState<ReadonlySet<string>>(new Set());
  const [bounds, setBounds] = useState<WorldBoundsDto | null>(null);
  const [biomeInstance, setBiomeInstance] = useState<string | null>(null);
  const [layers, setLayers] = useState<VegetationLayerDto[]>([]);
  const [epoch, setEpoch] = useState(0);
  const engineReady = useEditorStore((s) => s.engineStatus.phase === "ready");
  useEffect(() => {
    if (!engineReady) {
      return;
    }
    let live = true;
    let known: string | null = null;
    let timer: ReturnType<typeof setTimeout> | undefined;
    const tick = async () => {
      try {
        const status = await client.vegetationRuntimeStatus();
        const bound = status.state === "available" ? status.map : null;
        if (live) {
          known = bound;
          setMap(bound);
          if (bound !== null) {
            const summary = await client.vegetationAssetSummary(bound);
            if (live) {
              setLayers(summary.layers);
              const asset =
                summary.summary.kind === "vegetation-map"
                  ? (summary.summary.asset as {
                      generation: string;
                      chunkLevel: number;
                      dirtyLayers: string[];
                      bounds: WorldBoundsDto;
                      biomeInstances: { instance: string; biome: string }[];
                    })
                  : null;
              setGeneration(asset?.generation ?? null);
              setChunkLevel(asset?.chunkLevel ?? null);
              setDirtyLayers(new Set(asset?.dirtyLayers ?? []));
              setBounds(asset?.bounds ?? null);
              setBiomeInstance(asset?.biomeInstances[0]?.instance ?? null);
            }
          } else {
            setLayers([]);
            setGeneration(null);
            setChunkLevel(null);
            setDirtyLayers(new Set());
            setBounds(null);
            setBiomeInstance(null);
          }
        }
      } catch (err) {
        if (!isBusyLoading(err) && live && known !== null) {
          known = null;
          setMap(null);
          setLayers([]);
          setGeneration(null);
          setChunkLevel(null);
          setDirtyLayers(new Set());
          setBounds(null);
          setBiomeInstance(null);
        }
      }
      if (live) {
        timer = setTimeout(tick, 2000);
      }
    };
    void tick();
    return () => {
      live = false;
      if (timer !== undefined) {
        clearTimeout(timer);
      }
    };
  }, [engineReady, epoch]);
  return {
    map,
    generation,
    chunkLevel,
    dirtyLayers,
    bounds,
    biomeInstance,
    layers,
    refresh: () => setEpoch((value) => value + 1),
  };
}

/// Polls the store-published cook job at 1 Hz until it reaches a terminal state,
/// keeping the last snapshot on screen; a failed cook raises one toast.
function useVegetationCook(): VegetationCookStatusDto | null {
  const job = useEditorStore((s) => s.vegetationCookJob);
  const setJob = useEditorStore((s) => s.setVegetationCookJob);
  const engineReady = useEditorStore((s) => s.engineStatus.phase === "ready");
  const [status, setStatus] = useState<VegetationCookStatusDto | null>(null);
  useEffect(() => {
    if (!engineReady || job === null) {
      setStatus(null);
      return;
    }
    let live = true;
    let timer: ReturnType<typeof setTimeout> | undefined;
    let notified = false;
    const tick = async () => {
      try {
        const next = await client.vegetationCookStatus(job);
        if (!live) {
          return;
        }
        setStatus(next);
        if (next.state === "failed" && next.error && !notified) {
          notified = true;
          notifyError(next.error.message);
        }
        if (next.state === "queued" || next.state === "running") {
          timer = setTimeout(tick, 1000);
        }
      } catch (err) {
        if (!live) {
          return;
        }
        if (isBusyLoading(err)) {
          timer = setTimeout(tick, 1000);
        } else {
          setStatus(null);
          setJob(null);
        }
      }
    };
    void tick();
    return () => {
      live = false;
      if (timer !== undefined) {
        clearTimeout(timer);
      }
    };
  }, [engineReady, job, setJob]);
  return status;
}

/// The paint target a layer row resolves to. A density/scalar-field layer takes brush strokes into
/// its field tiles; a blocker layer takes them into the signed-blocker slot the evaluator reads;
/// a volume/spline layer takes an analytic shape instead and has no tile channel.
function paintTarget(
  map: string,
  chunkLevel: number,
  layer: VegetationLayerDto,
): VegetationPaintTarget {
  const operator = layer.operator;
  const channel =
    operator.kind === "density" || operator.kind === "scalar-field"
      ? operator.channel
      : operator.kind === "blocker"
        ? ({ kind: "signed-blocker" } as const)
        : null;
  return {
    map,
    layer: layer.id,
    operator: operator.kind,
    channel,
    slot: operator.kind === "blocker" ? "blocker" : "field",
    chunkLevel,
    locked: layer.locked,
  };
}

/// Polls the vegetation render population at 1 Hz while the panel is visible
/// (the registry mounts it `onlyWhenVisible`, so an unmounted panel costs nothing).
function useVegetationStats(): VegetationRenderStatsDto | null {
  const [stats, setStats] = useState<VegetationRenderStatsDto | null>(null);
  const engineReady = useEditorStore((s) => s.engineStatus.phase === "ready");
  useEffect(() => {
    if (!engineReady) {
      return;
    }
    let live = true;
    let timer: ReturnType<typeof setTimeout> | undefined;
    const tick = async () => {
      try {
        const next = await client.vegetationRenderStats();
        if (live) {
          setStats(next);
        }
      } catch (err) {
        if (!isBusyLoading(err) && live) {
          setStats(null);
        }
      }
      if (live) {
        timer = setTimeout(tick, 1000);
      }
    };
    void tick();
    return () => {
      live = false;
      if (timer !== undefined) {
        clearTimeout(timer);
      }
    };
  }, [engineReady]);
  return stats;
}

export function VegetationPanel() {
  const tool = useEditorStore((s) => s.vegetationTool);
  const brush = useEditorStore((s) => s.vegetationBrush);
  const setTool = useEditorStore((s) => s.setVegetationTool);
  const setBrush = useEditorStore((s) => s.setVegetationBrush);
  const species = useEditorStore((s) => s.vegetationSpecies);
  const toggleSpecies = useEditorStore((s) => s.toggleVegetationSpecies);
  const weights = useEditorStore((s) => s.vegetationWeights);
  const setWeight = useEditorStore((s) => s.setVegetationWeight);
  const [speciesQuery, setSpeciesQuery] = useState("");
  const plants = useEditorStore((s) => s.assets).filter((asset) => asset.type === "plant");
  const query = speciesQuery.trim().toLowerCase();
  const visiblePlants =
    query === "" ? plants : plants.filter((plant) => plant.name.toLowerCase().includes(query));
  const stats = useVegetationStats();
  const { map, chunkLevel, dirtyLayers, bounds, biomeInstance, layers, refresh } =
    useVegetationMap();
  const cookStatus = useVegetationCook();
  const setCookJob = useEditorStore((s) => s.setVegetationCookJob);
  const lastStroke = useEditorStore((s) => s.vegetationLastStroke);
  const [preflight, setPreflight] = useState<VegetationEvaluationPreflightDto | null>(null);
  const [estimating, setEstimating] = useState(false);
  const [topologyDiff, setTopologyDiff] = useState<VegetationTopologyDiffResult | null>(null);
  // The manifest identities of the two most recent completed cooks — the Review
  // action diffs previous → current.
  const cookIdentities = useRef<{ previous: string | null; current: string | null }>({
    previous: null,
    current: null,
  });
  const completedIdentity =
    cookStatus?.state === "completed" ? (cookStatus.manifest?.identity ?? null) : null;
  useEffect(() => {
    if (completedIdentity !== null && completedIdentity !== cookIdentities.current.current) {
      cookIdentities.current = {
        previous: cookIdentities.current.current,
        current: completedIdentity,
      };
      setTopologyDiff(null);
    }
  }, [completedIdentity]);

  /// Diffs the two most recent cooked manifests (added/removed/moved plants and
  /// unresolved authored overrides per changed cell).
  const reviewChanges = (): void => {
    const { previous, current } = cookIdentities.current;
    if (map === null || previous === null || current === null) {
      return;
    }
    client
      .vegetationTopologyDiff({ map, from: previous, to: current })
      .then(setTopologyDiff)
      .catch((err: unknown) => notifyError(errorText(err)));
  };

  /// Preflights the last stroke region (the whole map before any stroke) through the
  /// symbolic evaluation admission — predicted candidates/accepted/micro samples and
  /// the peak memory bound — then cancels the retained job.
  const runEstimate = (): void => {
    const region = lastStroke ?? bounds;
    if (map === null || biomeInstance === null || chunkLevel === null || region === null) {
      return;
    }
    setEstimating(true);
    void (async () => {
      try {
        const prepared = await client.vegetationPreflightRegion({
          map,
          biomeInstance,
          bounds: region,
          level: chunkLevel,
        });
        setPreflight(prepared.preflight);
        await client.vegetationCancelEvaluation(prepared.job);
      } catch (err) {
        notifyError(errorText(err));
      } finally {
        setEstimating(false);
      }
    })();
  };
  const pushEdit = useEditorStore((s) => s.pushEdit);
  const editing = useEditorStore((s) => s.playState === "edit");
  const activeLayer = useEditorStore((s) => s.vegetationActiveLayer);
  const setActiveLayer = useEditorStore((s) => s.setVegetationActiveLayer);
  // The layer row a drag picked up, and the row it currently hovers (the drop indicator).
  const dragLayer = useRef<string | null>(null);
  const [dropIndex, setDropIndex] = useState<number | null>(null);

  // Keeps the stroke target current: a lock/mute edit, a map change, or a deleted
  // layer re-derives (or clears) the stored target; the identity-stable setter
  // makes the re-derive a no-op when nothing changed.
  useEffect(() => {
    const active = useEditorStore.getState().vegetationActiveLayer;
    if (!active) {
      return;
    }
    const row = layers.find((layer) => layer.id === active.layer);
    if (map === null || chunkLevel === null || !row) {
      setActiveLayer(null);
      return;
    }
    setActiveLayer(paintTarget(map, chunkLevel, row));
  }, [map, chunkLevel, layers, setActiveLayer]);

  /// Re-reads the map's current generation and rows, applies `patch` over the fresh
  /// rows, and commits every touched row with a bumped revision — one optimistic
  /// transaction per call, safe for undo/redo replay because nothing is captured stale.
  const commitLayersPatch = async (
    patch: (rows: VegetationLayerDto[]) => Map<string, Partial<VegetationLayerDto>>,
  ): Promise<void> => {
    if (map === null) {
      return;
    }
    const summary = await client.vegetationAssetSummary(map);
    if (summary.summary.kind !== "vegetation-map") {
      return;
    }
    const patches = patch(summary.layers);
    const upserts = summary.layers
      .filter((row) => patches.has(row.id))
      .map((row) => ({
        ...row,
        ...patches.get(row.id),
        revision: (BigInt(row.revision) + 1n).toString(),
      }));
    if (upserts.length === 0) {
      return;
    }
    await client.vegetationMapLayerCommit({
      map,
      expectedGeneration: (summary.summary.asset as { generation: string }).generation,
      upserts,
      removals: [],
    });
    refresh();
  };

  const toggleLayerFlag = (layer: VegetationLayerDto, flag: "muted" | "locked"): void => {
    const from = layer[flag];
    const apply = (value: boolean) => () =>
      commitLayersPatch(
        (rows) =>
          new Map(
            rows
              .filter((row) => row.id === layer.id && row[flag] !== value)
              .map((row): [string, Partial<VegetationLayerDto>] => [row.id, { [flag]: value }]),
          ),
      );
    void apply(!from)()
      .then(() =>
        pushEdit({
          label: flag === "muted" ? "Toggle layer mute" : "Toggle layer lock",
          undo: apply(from),
          redo: apply(!from),
        }),
      )
      .catch((err: unknown) => notifyError(errorText(err)));
  };

  /// Solo mutes every other layer in one transaction; a second solo on the already
  /// soloed layer unmutes all. Undo restores the mute states captured at click time.
  const soloLayer = (target: VegetationLayerDto): void => {
    const before = new Map(layers.map((row): [string, boolean] => [row.id, row.muted]));
    const soloed = layers.every((row) => (row.id === target.id ? !row.muted : row.muted));
    const apply = () =>
      commitLayersPatch(
        (rows) =>
          new Map(
            rows
              .filter((row) => row.muted !== (soloed ? false : row.id !== target.id))
              .map((row): [string, Partial<VegetationLayerDto>] => [
                row.id,
                { muted: soloed ? false : row.id !== target.id },
              ]),
          ),
      );
    const restore = () =>
      commitLayersPatch(
        (rows) =>
          new Map(
            rows
              .filter((row) => before.has(row.id) && before.get(row.id) !== row.muted)
              .map((row): [string, Partial<VegetationLayerDto>] => [
                row.id,
                { muted: before.get(row.id) ?? false },
              ]),
          ),
      );
    void apply()
      .then(() => pushEdit({ label: "Solo layer", undo: restore, redo: apply }))
      .catch((err: unknown) => notifyError(errorText(err)));
  };

  /// Moves one layer to `toIndex` in evaluation order and renumbers the run it passed through —
  /// one transaction over every row whose order changed. Both the drag gesture and the up/down
  /// buttons come here; undo replays the captured prior order set.
  const reorderLayer = (layerId: string, toIndex: number): void => {
    const sorted = [...layers].sort((a, b) => a.order - b.order);
    const fromIndex = sorted.findIndex((row) => row.id === layerId);
    if (fromIndex < 0 || toIndex < 0 || toIndex >= sorted.length || fromIndex === toIndex) {
      return;
    }
    const moved = [...sorted];
    moved.splice(toIndex, 0, moved.splice(fromIndex, 1)[0]!);
    const before = new Map(sorted.map((row): [string, number] => [row.id, row.order]));
    const after = new Map(moved.map((row, index): [string, number] => [row.id, index]));
    const apply = (orders: ReadonlyMap<string, number>) => () =>
      commitLayersPatch(
        (rows) =>
          new Map(
            rows
              .filter((row) => orders.has(row.id) && orders.get(row.id) !== row.order)
              .map((row): [string, Partial<VegetationLayerDto>] => [
                row.id,
                { order: orders.get(row.id)! },
              ]),
          ),
      );
    void apply(after)()
      .then(() => pushEdit({ label: "Reorder layer", undo: apply(before), redo: apply(after) }))
      .catch((err: unknown) => notifyError(errorText(err)));
  };
  const brushTool = isBrushTool(tool);
  const falloffTool = usesFalloff(tool);
  const strokeTool = isStrokeTool(tool);
  const densityTool = usesTargetDensity(tool);
  const orderedLayers = [...layers].sort((a, b) => a.order - b.order);

  return (
    <div className="flex h-full min-h-0 flex-col bg-background text-foreground">
      <ScrollArea className="min-h-0 flex-1">
        <div className="flex flex-col gap-3 p-2">
          <div className="flex flex-col gap-1.5">
            <SectionLabel>Tools</SectionLabel>
            <div className="grid grid-cols-5 gap-1">
              {VEGETATION_TOOLS.map(({ tool: id, label, icon: Icon }) => {
                const disabled = !editing && !isRuntimeTool(id);
                return (
                  <Tooltip key={id}>
                    <TooltipTrigger asChild>
                      <Button
                        size="icon"
                        variant={tool === id ? "default" : "ghost"}
                        aria-pressed={tool === id}
                        aria-label={label}
                        className="h-7 w-full"
                        disabled={disabled}
                        onClick={() => setTool(id)}
                      >
                        <Icon className="h-3.5 w-3.5" />
                      </Button>
                    </TooltipTrigger>
                    <TooltipContent side="bottom">
                      {disabled ? `${label} authors the map — stop play to use it` : label}
                    </TooltipContent>
                  </Tooltip>
                );
              })}
            </div>
          </div>

          {brushTool && (
            <div className="flex flex-col gap-1.5">
              <SectionLabel>Brush</SectionLabel>
              <BrushField label="Radius (m)">
                <SliderField
                  value={brush.radius}
                  min={0.5}
                  max={64}
                  step={0.5}
                  onChange={(radius) => setBrush({ radius })}
                />
              </BrushField>
              {falloffTool && (
                <BrushField label="Falloff">
                  <SliderField
                    value={brush.falloff}
                    min={0}
                    max={1}
                    step={0.01}
                    onChange={(falloff) => setBrush({ falloff })}
                  />
                </BrushField>
              )}
              {strokeTool && (
                <>
                  <BrushField label="Spacing (m)">
                    <SliderField
                      value={brush.spacing}
                      min={0.1}
                      max={16}
                      step={0.1}
                      onChange={(spacing) => setBrush({ spacing })}
                    />
                  </BrushField>
                  <BrushField label="Projection">
                    <Select
                      value={brush.projection}
                      onValueChange={(projection) =>
                        setBrush({ projection: projection as "view" | "down" })
                      }
                    >
                      <SelectTrigger size="sm" className="h-6 w-full text-[11px]">
                        <SelectValue />
                      </SelectTrigger>
                      <SelectContent>
                        <SelectItem value="view" className="text-[11px]">
                          View ray
                        </SelectItem>
                        <SelectItem value="down" className="text-[11px]">
                          Straight down
                        </SelectItem>
                      </SelectContent>
                    </Select>
                  </BrushField>
                  <BrushField label="Max slope (°)">
                    <SliderField
                      value={brush.maxSlopeDeg}
                      min={0}
                      max={90}
                      step={1}
                      onChange={(maxSlopeDeg) => setBrush({ maxSlopeDeg })}
                    />
                  </BrushField>
                </>
              )}
            </div>
          )}

          {densityTool && (
            <div className="flex flex-col gap-1.5">
              <SectionLabel>Target density</SectionLabel>
              <BrushField label="Level">
                <SliderField
                  value={brush.density}
                  min={0}
                  max={1}
                  step={0.01}
                  onChange={(density) => setBrush({ density })}
                />
              </BrushField>
            </div>
          )}

          <Separator />

          <div className="flex flex-col gap-1.5">
            <SectionLabel>Species</SectionLabel>
            {plants.length === 0 ? (
              <p className="text-[11px] text-muted-foreground">
                No plant assets in the project. Import a .splant package to paint with it.
              </p>
            ) : (
              <div className="flex flex-col gap-0.5">
                <Input
                  value={speciesQuery}
                  placeholder="Filter species…"
                  className="h-6 px-1.5 text-[11px]"
                  onChange={(event) => setSpeciesQuery(event.target.value)}
                />
                {visiblePlants.length === 0 ? (
                  <p className="text-[11px] text-muted-foreground">No species match the filter.</p>
                ) : (
                  visiblePlants.map((plant) => (
                    <div key={plant.id} className="rounded bg-card">
                      <button
                        type="button"
                        aria-pressed={species.has(plant.id)}
                        className={`flex w-full items-center gap-1.5 rounded px-1.5 py-1 text-left text-[11px] ${
                          species.has(plant.id)
                            ? "bg-primary text-primary-foreground"
                            : "text-foreground hover:bg-muted"
                        }`}
                        onClick={(event) => toggleSpecies(plant.id, event.ctrlKey || event.metaKey)}
                      >
                        <SpeciesThumb id={plant.id} name={plant.name} />
                        <span className="min-w-0 truncate">{plant.name}</span>
                      </button>
                      {species.has(plant.id) && (
                        <div className="flex items-center gap-2 px-1.5 py-1">
                          <span className="w-12 shrink-0 text-[10px] text-muted-foreground">
                            Weight
                          </span>
                          <div className="min-w-0 flex-1">
                            <SliderField
                              value={weights[plant.id] ?? 1}
                              min={0}
                              max={1}
                              step={0.01}
                              onChange={(weight) => setWeight(plant.id, weight)}
                            />
                          </div>
                        </div>
                      )}
                    </div>
                  ))
                )}
              </div>
            )}
          </div>

          <div className="flex flex-col gap-1.5">
            <SectionLabel>Layers</SectionLabel>
            {map === null ? (
              <p className="text-[11px] text-muted-foreground">No vegetation map is bound.</p>
            ) : layers.length === 0 ? (
              <p className="text-[11px] text-muted-foreground">The bound map has no layers.</p>
            ) : (
              <div className="flex flex-col gap-0.5">
                {orderedLayers.map((layer, index, ordered) => (
                  <div
                    key={layer.id}
                    draggable
                    onDragStart={(event) => {
                      dragLayer.current = layer.id;
                      event.dataTransfer.effectAllowed = "move";
                    }}
                    onDragOver={(event) => {
                      if (dragLayer.current === null || dragLayer.current === layer.id) {
                        return;
                      }
                      event.preventDefault();
                      event.dataTransfer.dropEffect = "move";
                      setDropIndex(index);
                    }}
                    onDragLeave={() => setDropIndex((at) => (at === index ? null : at))}
                    onDragEnd={() => {
                      dragLayer.current = null;
                      setDropIndex(null);
                    }}
                    onDrop={(event) => {
                      event.preventDefault();
                      const dragged = dragLayer.current;
                      dragLayer.current = null;
                      setDropIndex(null);
                      if (dragged !== null) {
                        reorderLayer(dragged, index);
                      }
                    }}
                    className={`flex items-center justify-between gap-2 rounded px-1.5 py-1 ${
                      dropIndex === index ? "outline outline-1 outline-primary" : ""
                    } ${activeLayer?.layer === layer.id ? "bg-primary/20" : "bg-card"}`}
                  >
                    <button
                      type="button"
                      aria-pressed={activeLayer?.layer === layer.id}
                      className="min-w-0 flex-1 truncate text-left text-[11px] hover:text-foreground"
                      onClick={() =>
                        chunkLevel !== null &&
                        map !== null &&
                        setActiveLayer(
                          activeLayer?.layer === layer.id
                            ? null
                            : paintTarget(map, chunkLevel, layer),
                        )
                      }
                    >
                      {layer.name}
                    </button>
                    <span className="flex shrink-0 items-center gap-1">
                      {dirtyLayers.has(layer.id) && (
                        <Tooltip>
                          <TooltipTrigger asChild>
                            <span
                              aria-label="Uncooked edits"
                              className="h-1.5 w-1.5 rounded-full bg-amber-500"
                            />
                          </TooltipTrigger>
                          <TooltipContent side="bottom">
                            Authored edits not yet cooked
                          </TooltipContent>
                        </Tooltip>
                      )}
                      <span className="font-mono text-[10px] text-muted-foreground">
                        #{layer.order}
                      </span>
                      <Button
                        size="icon"
                        variant="ghost"
                        aria-label="Move layer earlier"
                        className="h-5 w-5"
                        disabled={index === 0}
                        onClick={() => reorderLayer(layer.id, index - 1)}
                      >
                        <ChevronUp className="h-3 w-3" />
                      </Button>
                      <Button
                        size="icon"
                        variant="ghost"
                        aria-label="Move layer later"
                        className="h-5 w-5"
                        disabled={index === ordered.length - 1}
                        onClick={() => reorderLayer(layer.id, index + 1)}
                      >
                        <ChevronDown className="h-3 w-3" />
                      </Button>
                      <Button
                        size="icon"
                        variant="ghost"
                        aria-label="Solo layer"
                        className="h-5 w-5"
                        onClick={() => soloLayer(layer)}
                      >
                        <Headphones className="h-3 w-3" />
                      </Button>
                      <Button
                        size="icon"
                        variant={layer.muted ? "default" : "ghost"}
                        aria-pressed={layer.muted}
                        aria-label={layer.muted ? "Unmute layer" : "Mute layer"}
                        className="h-5 w-5"
                        onClick={() => toggleLayerFlag(layer, "muted")}
                      >
                        {layer.muted ? (
                          <VolumeX className="h-3 w-3" />
                        ) : (
                          <Volume2 className="h-3 w-3" />
                        )}
                      </Button>
                      <Button
                        size="icon"
                        variant={layer.locked ? "default" : "ghost"}
                        aria-pressed={layer.locked}
                        aria-label={layer.locked ? "Unlock layer" : "Lock layer"}
                        className="h-5 w-5"
                        onClick={() => toggleLayerFlag(layer, "locked")}
                      >
                        {layer.locked ? (
                          <Lock className="h-3 w-3" />
                        ) : (
                          <LockOpen className="h-3 w-3" />
                        )}
                      </Button>
                    </span>
                  </div>
                ))}
              </div>
            )}
          </div>

          {map !== null && biomeInstance !== null && (
            <div className="flex flex-col gap-1.5">
              <SectionLabel>Estimate</SectionLabel>
              <Button
                size="sm"
                variant="outline"
                className="h-6 text-[11px]"
                disabled={estimating}
                onClick={runEstimate}
              >
                {lastStroke !== null ? "Estimate stroke region" : "Estimate map"}
              </Button>
              {preflight !== null && (
                <div className="flex flex-col gap-1">
                  <Stat label="Predicted candidates" value={preflight.candidateCount} />
                  <Stat label="Predicted accepted" value={preflight.acceptedCount} />
                  <Stat label="Micro samples" value={preflight.microSamples} />
                  <Stat
                    label="Peak memory"
                    value={`${(Number(preflight.preflightPeakBytes) / (1024 * 1024)).toFixed(1)} MB`}
                  />
                </div>
              )}
            </div>
          )}

          <div className="flex flex-col gap-1.5">
            <SectionLabel>Cook</SectionLabel>
            <div className="flex items-center gap-2">
              <Button
                size="sm"
                variant="outline"
                className="h-6 flex-1 text-[11px]"
                disabled={
                  map === null || cookStatus?.state === "queued" || cookStatus?.state === "running"
                }
                onClick={() => {
                  if (map === null) {
                    return;
                  }
                  client
                    .vegetationCook({ map, scope: { kind: "all" } })
                    .then((job) => setCookJob(job.job))
                    .catch((err: unknown) => notifyError(errorText(err)));
                }}
              >
                Cook map
              </Button>
              {(cookStatus?.state === "queued" || cookStatus?.state === "running") && (
                <Button
                  size="sm"
                  variant="ghost"
                  className="h-6 text-[11px]"
                  onClick={() =>
                    void client
                      .vegetationCancelCook(cookStatus.job)
                      .catch((err: unknown) => notifyError(errorText(err)))
                  }
                >
                  Cancel
                </Button>
              )}
            </div>
            {cookStatus !== null && (
              <div className="flex flex-col gap-1">
                <Stat label="State" value={cookStatus.state} />
                <Stat
                  label="Nodes"
                  value={`${cookStatus.progress.completedNodes}/${cookStatus.progress.totalNodes}`}
                />
                <Stat label="Published cells" value={cookStatus.progress.publishedCells} />
              </div>
            )}
            {cookStatus?.state === "completed" && cookIdentities.current.previous !== null && (
              <Button size="sm" variant="ghost" className="h-6 text-[11px]" onClick={reviewChanges}>
                Review changes
              </Button>
            )}
            {topologyDiff !== null && (
              <div className="flex flex-col gap-1">
                <Stat label="Changed cells" value={topologyDiff.cells.length} />
                <Stat
                  label="Added / removed / moved"
                  value={topologyDiff.cells.reduce(
                    (sums, cell) =>
                      `${Number(sums.split(" / ")[0]) + Number(cell.added)} / ${
                        Number(sums.split(" / ")[1]) + Number(cell.removed)
                      } / ${Number(sums.split(" / ")[2]) + Number(cell.moved)}`,
                    "0 / 0 / 0",
                  )}
                />
                {topologyDiff.cells.flatMap((cell) => cell.conflicts).length > 0 && (
                  <div className="flex flex-col gap-0.5">
                    <span className="text-[10px] text-amber-500">
                      Unresolved overrides (recook keeps them until their plants return)
                    </span>
                    {topologyDiff.cells
                      .flatMap((cell) => cell.conflicts)
                      .slice(0, 12)
                      .map((conflict) => (
                        <span
                          key={`${conflict.kind}-${conflict.plant}`}
                          className="font-mono text-[10px] text-muted-foreground"
                        >
                          {conflict.kind} · …{conflict.plant.slice(-8)}
                        </span>
                      ))}
                  </div>
                )}
              </div>
            )}
          </div>

          <Separator />

          <div className="flex flex-col gap-1.5">
            <SectionLabel>Resident population</SectionLabel>
            {stats === null ? (
              <p className="text-[11px] text-muted-foreground">
                No vegetation is streaming. Bind a vegetation map to a VegetationField component to
                populate this panel.
              </p>
            ) : (
              <div className="flex flex-col gap-1">
                {stats.families.length === 0 ? (
                  <p className="text-[11px] text-muted-foreground">No resident families.</p>
                ) : (
                  stats.families.map((family: VegetationFamilyRenderDto) => (
                    <div
                      key={family.family}
                      className="flex items-baseline justify-between gap-2 rounded bg-card px-1.5 py-1"
                    >
                      <span className="font-mono text-[11px] text-muted-foreground">
                        {shortFamily(family.family)}
                      </span>
                      <span className="font-mono text-[11px] tabular-nums">
                        {family.instances} plants · {family.fieldTiles} tiles
                      </span>
                    </div>
                  ))
                )}
                <Stat label="Resident cells" value={stats.cells.length} />
                <Stat
                  label="Predicted blades"
                  value={stats.families.reduce(
                    (sum: number, family: VegetationFamilyRenderDto) =>
                      sum + Number(family.microPredicted),
                    0,
                  )}
                />
                <Stat label="Page faults" value={stats.pageFaults} />
              </div>
            )}
          </div>
        </div>
      </ScrollArea>
    </div>
  );
}
