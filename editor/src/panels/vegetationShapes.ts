/// Analytic-shape authoring: the Volume and Spline tools write their gesture into the active
/// layer's operator through one optimistic `vegetation-map-layer-commit` transaction, then recook
/// the region the shape covers. A shape layer holds exactly one shape, so a gesture replaces that
/// layer's shape rather than minting a layer the biome graph does not read.
import { client } from "../control/client";
import type { VegetationLayerDto, WorldBoundsDto } from "../protocol";
import type { VegetationPaintTarget, VegetationPoint } from "../state/store";

/// Q15.16 metres, the unit every analytic shape dimension crosses the wire in.
const Q16 = 65536;
const TICKS_PER_METER = 4096;

/// An undoable pair of control calls, the shape the caller records with `pushEdit`.
export interface ShapeEdit {
  undo: () => Promise<unknown>;
  redo: () => Promise<unknown>;
}

function ticks(meters: number): string {
  return String(Math.round(meters * TICKS_PER_METER));
}

/// The half-open world bounds spanned by `points`, padded by `pad` metres on every axis.
export function boundsAround(points: VegetationPoint[], pad: number): WorldBoundsDto {
  const min = [Infinity, Infinity, Infinity];
  const max = [-Infinity, -Infinity, -Infinity];
  for (const point of points) {
    for (let axis = 0; axis < 3; axis += 1) {
      min[axis] = Math.min(min[axis]!, point[axis]! - pad);
      max[axis] = Math.max(max[axis]!, point[axis]! + pad);
    }
  }
  return {
    minTicks: [ticks(min[0]!), ticks(min[1]!), ticks(min[2]!)],
    maxTicksExclusive: [ticks(max[0]! + 1), ticks(max[1]! + 1), ticks(max[2]! + 1)],
  };
}

/// Re-reads the map's rows, applies `patch` to the target layer, and commits it with a bumped
/// revision under the freshly-read generation. Nothing is captured stale, so the same call is safe
/// to replay from an undo entry.
async function commitLayer(
  target: VegetationPaintTarget,
  patch: (row: VegetationLayerDto) => VegetationLayerDto,
): Promise<void> {
  const summary = await client.vegetationAssetSummary(target.map);
  if (summary.summary.kind !== "vegetation-map") {
    return;
  }
  const row = summary.layers.find((layer) => layer.id === target.layer);
  if (!row) {
    return;
  }
  const patched = patch(row);
  await client.vegetationMapLayerCommit({
    map: target.map,
    expectedGeneration: (summary.summary.asset as { generation: string }).generation,
    upserts: [{ ...patched, revision: (BigInt(row.revision) + 1n).toString() }],
    removals: [],
  });
}

/// Recooks every cell the shape's bounds reach at the map's chunk level.
async function cookBounds(target: VegetationPaintTarget, bounds: WorldBoundsDto): Promise<void> {
  await client.vegetationCook({
    map: target.map,
    scope: { kind: "bounds", bounds, level: target.chunkLevel },
    workers: 1,
  });
}

/// Reads the layer row the shape tools write into, or null when the map no longer carries it.
async function layerRow(target: VegetationPaintTarget): Promise<VegetationLayerDto | null> {
  const summary = await client.vegetationAssetSummary(target.map);
  if (summary.summary.kind !== "vegetation-map") {
    return null;
  }
  return summary.layers.find((layer) => layer.id === target.layer) ?? null;
}

/// Restores one layer's operator and bounds — the inverse half of every shape gesture.
function restoreShape(target: VegetationPaintTarget, prior: VegetationLayerDto) {
  return async (): Promise<unknown> => {
    await commitLayer(target, (row) => ({
      ...row,
      bounds: prior.bounds,
      operator: prior.operator,
    }));
    return cookBounds(target, prior.bounds);
  };
}

/// Writes the dragged box into the active volume layer: the box the two picked ground points span,
/// raised `height` metres, with the brush falloff as its soft edge. The include/exclude sense the
/// layer already carries is kept — that is an authored decision about the layer, not about one drag.
export async function commitVolume(
  target: VegetationPaintTarget,
  corners: [VegetationPoint, VegetationPoint],
  height: number,
  falloff: number,
): Promise<ShapeEdit | null> {
  if (target.operator !== "volume" || target.locked) {
    return null;
  }
  const prior = await layerRow(target);
  if (!prior || prior.operator.kind !== "volume") {
    return null;
  }
  const lifted: VegetationPoint[] = [
    [corners[0][0], corners[0][1], corners[0][2]],
    [corners[1][0], corners[1][1] + height, corners[1][2]],
  ];
  const bounds = boundsAround(lifted, 0);
  const operation = prior.operator.operation;
  const apply = async (): Promise<unknown> => {
    await commitLayer(target, (row) => ({
      ...row,
      bounds,
      operator: {
        kind: "volume",
        bounds,
        operation,
        falloffBits: Math.round(falloff * Q16),
      },
    }));
    return cookBounds(target, bounds);
  };
  await apply();
  return { undo: restoreShape(target, prior), redo: apply };
}

/// Writes the picked control polyline into the active spline layer at the brush radius. The spline
/// identity the layer already carries is kept, so the influence keeps its provenance across edits.
export async function commitSpline(
  target: VegetationPaintTarget,
  points: VegetationPoint[],
  radius: number,
): Promise<ShapeEdit | null> {
  if (target.operator !== "spline" || target.locked || points.length < 2) {
    return null;
  }
  const prior = await layerRow(target);
  if (!prior || prior.operator.kind !== "spline") {
    return null;
  }
  const bounds = boundsAround(points, radius);
  const spline = prior.operator.spline;
  const operation = prior.operator.operation;
  const wire = points.map(
    (point) => [ticks(point[0]), ticks(point[1]), ticks(point[2])] as [string, string, string],
  );
  const apply = async (): Promise<unknown> => {
    await commitLayer(target, (row) => ({
      ...row,
      bounds,
      operator: {
        kind: "spline",
        spline,
        points: wire,
        radiusBits: Math.round(radius * Q16),
        operation,
      },
    }));
    return cookBounds(target, bounds);
  };
  await apply();
  return { undo: restoreShape(target, prior), redo: apply };
}
