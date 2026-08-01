/// Brush-stroke synthesis: rasterizes a captured stroke's stamps into authored
/// field tiles and commits every touched chunk as one optimistic
/// `vegetation-map-chunk-commit` transaction (read-modify-write over
/// `vegetation-map-chunk-read`). Undo restores the pre-stroke chunk payloads;
/// a chunk the stroke created is removed again.
import { client } from "../control/client";
import type {
  AuthoredFieldTileDto,
  VegetationMapChunkDto,
  VegetationMapChunkKeyDto,
} from "../protocol";
import { useEditorStore, type VegetationPaintTarget } from "../state/store";

/// Ticks per metre and level-zero cell edge ticks (64 m cells, scaled by level).
const TICKS_PER_METER = 4096n;
const CELL_TICKS = 64n * TICKS_PER_METER;

/// Painted-tile raster defaults for a fresh chunk: a horizontal density grid at
/// one texel per metre of a level-zero cell, quantized so 256 steps span 0..1.
const DEFAULT_DIMENSIONS: [number, number, number] = [64, 1, 64];
const DEFAULT_QUANTUM_BITS = 256;
const UNIT_Q16 = 65536;

/// How one stamp combines with the texels it covers: `add` accumulates `sign · weight`, `level`
/// drives each texel toward `level` by `weight` (what the Density tool paints).
export type BrushBlend = { kind: "add"; sign: 1 | -1 } | { kind: "level"; level: number };

/// One brush stamp along the stroke path (world metres).
export interface BrushStamp {
  position: [number, number, number];
  radius: number;
  falloff: number;
  /// Pointer pressure 0..1 scaling the stamp's contribution.
  pressure: number;
  blend: BrushBlend;
}

function floorDiv(value: bigint, divisor: bigint): bigint {
  const quotient = value / divisor;
  return value % divisor !== 0n && value < 0n !== divisor < 0n ? quotient - 1n : quotient;
}

/// The chunk cell holding a world position at the map's chunk level.
export function chunkCell(
  position: [number, number, number],
  level: number,
): [bigint, bigint, bigint] {
  const edge = CELL_TICKS << BigInt(level);
  return [
    floorDiv(BigInt(Math.round(position[0] * 4096)), edge),
    floorDiv(BigInt(Math.round(position[1] * 4096)), edge),
    floorDiv(BigInt(Math.round(position[2] * 4096)), edge),
  ];
}

function cellKeyString(cell: [bigint, bigint, bigint]): string {
  return `${cell[0]},${cell[1]},${cell[2]}`;
}

export function chunkKey(target: VegetationPaintTarget, cell: [bigint, bigint, bigint]) {
  return {
    layer: target.layer,
    tile: {
      kind: "cell" as const,
      cell: {
        coordinates: [cell[0].toString(), cell[1].toString(), cell[2].toString()] as [
          string,
          string,
          string,
        ],
        level: target.chunkLevel,
      },
    },
    kind: "field" as const,
  };
}

/// One editable tile grid: densities in 0..1 over the chunk cell's horizontal plane.
interface TileGrid {
  dimensions: [number, number, number];
  quantumBits: number;
  densities: Float64Array;
}

function gridFromTile(tile: AuthoredFieldTileDto): TileGrid {
  const densities = new Float64Array(tile.values.length);
  for (let index = 0; index < tile.values.length; index += 1) {
    densities[index] = (tile.values[index]! * tile.quantumBits) / UNIT_Q16;
  }
  return { dimensions: tile.dimensions, quantumBits: tile.quantumBits, densities };
}

function freshGrid(): TileGrid {
  return {
    dimensions: DEFAULT_DIMENSIONS,
    quantumBits: DEFAULT_QUANTUM_BITS,
    densities: new Float64Array(
      DEFAULT_DIMENSIONS[0] * DEFAULT_DIMENSIONS[1] * DEFAULT_DIMENSIONS[2],
    ),
  };
}

function tileFromGrid(target: VegetationPaintTarget, grid: TileGrid): AuthoredFieldTileDto {
  const values: number[] = new Array(grid.densities.length);
  for (let index = 0; index < grid.densities.length; index += 1) {
    const clamped = Math.min(1, Math.max(0, grid.densities[index]!));
    values[index] = Math.round((clamped * UNIT_Q16) / grid.quantumBits);
  }
  if (target.channel === null) {
    throw new Error("the active vegetation layer takes no strokes");
  }
  return {
    channel: target.channel,
    layer: target.layer,
    dimensions: grid.dimensions,
    quantumBits: grid.quantumBits,
    values,
  };
}

/// Applies one weighted sample to a texel under the stamp's blend.
function blendTexel(current: number, weight: number, blend: BrushBlend): number {
  return blend.kind === "add"
    ? current + blend.sign * weight
    : current + (blend.level - current) * Math.min(1, weight);
}

/// Splats one stamp into a cell's grid and reports whether any texel changed.
/// The grid spans the cell's horizontal bounds in the engine's packed order
/// (`(x · dimY + y) · dimZ + z`, y collapsed to one slab); the weight is 1 inside
/// the un-falloff'd core and fades linearly to the radius edge.
function splat(
  grid: TileGrid,
  cell: [bigint, bigint, bigint],
  level: number,
  stamp: BrushStamp,
): boolean {
  const edgeMeters = 64 * 2 ** level;
  const originX = Number(cell[0]) * edgeMeters;
  const originZ = Number(cell[2]) * edgeMeters;
  const [dimX, dimY, dimZ] = grid.dimensions;
  const texelX = edgeMeters / dimX;
  const texelZ = edgeMeters / dimZ;
  const minX = Math.max(0, Math.floor((stamp.position[0] - stamp.radius - originX) / texelX));
  const maxX = Math.min(dimX - 1, Math.ceil((stamp.position[0] + stamp.radius - originX) / texelX));
  const minZ = Math.max(0, Math.floor((stamp.position[2] - stamp.radius - originZ) / texelZ));
  const maxZ = Math.min(dimZ - 1, Math.ceil((stamp.position[2] + stamp.radius - originZ) / texelZ));
  const inner = Math.max(0, 1 - stamp.falloff);
  let touched = false;
  for (let z = minZ; z <= maxZ; z += 1) {
    for (let x = minX; x <= maxX; x += 1) {
      const centerX = originX + (x + 0.5) * texelX;
      const centerZ = originZ + (z + 0.5) * texelZ;
      const distance = Math.hypot(centerX - stamp.position[0], centerZ - stamp.position[2]);
      const normalized = distance / stamp.radius;
      if (normalized > 1) {
        continue;
      }
      const weight =
        (normalized <= inner || inner >= 1
          ? 1
          : Math.max(0, 1 - (normalized - inner) / (1 - inner))) * stamp.pressure;
      const index = x * dimY * dimZ + z;
      grid.densities[index] = blendTexel(grid.densities[index]!, weight, stamp.blend);
      touched = true;
    }
  }
  return touched;
}

interface ChunkReadResult {
  generation: string;
  chunks: VegetationMapChunkDto[];
}

/// The stroke's world bounds in ticks (stamp reach ± radius, one metre of vertical
/// headroom each way) — the region the panel's Estimate preflights.
function strokeBounds(stamps: BrushStamp[]): {
  minTicks: [string, string, string];
  maxTicksExclusive: [string, string, string];
} {
  const min = [Infinity, Infinity, Infinity];
  const max = [-Infinity, -Infinity, -Infinity];
  for (const stamp of stamps) {
    const reach = [stamp.radius, 1, stamp.radius];
    for (let axis = 0; axis < 3; axis += 1) {
      min[axis] = Math.min(min[axis]!, stamp.position[axis]! - reach[axis]!);
      max[axis] = Math.max(max[axis]!, stamp.position[axis]! + reach[axis]!);
    }
  }
  const ticks = (meters: number) => String(Math.round(meters * 4096));
  return {
    minTicks: [ticks(min[0]!), ticks(min[1]!), ticks(min[2]!)],
    maxTicksExclusive: [ticks(max[0]! + 1), ticks(max[1]! + 1), ticks(max[2]! + 1)],
  };
}

/// Queues a cell-scoped recook so the committed stroke manifests in the world,
/// publishing the job id for the panel's progress/cancel row.
export async function cookCells(map: string, keys: VegetationMapChunkKeyDto[]): Promise<void> {
  const cells = keys.flatMap((key) => (key.tile.kind === "cell" ? [key.tile.cell] : []));
  if (cells.length === 0) {
    return;
  }
  const job = await client.vegetationCook({ map, scope: { kind: "cells", cells }, workers: 1 });
  useEditorStore.getState().setVegetationCookJob(job.job);
}

/// Recooks the chunk cells a reapply stroke touched — no authored mutation, so the
/// deterministic pipeline reproduces the same output unless authored inputs changed;
/// the gesture refreshes a region after external edits.
export async function recookRegion(
  target: VegetationPaintTarget,
  stamps: BrushStamp[],
): Promise<void> {
  const cells = new Map<string, [bigint, bigint, bigint]>();
  for (const stamp of stamps) {
    for (const dx of [-stamp.radius, 0, stamp.radius]) {
      for (const dz of [-stamp.radius, 0, stamp.radius]) {
        const cell = chunkCell(
          [stamp.position[0] + dx, stamp.position[1], stamp.position[2] + dz],
          target.chunkLevel,
        );
        cells.set(cellKeyString(cell), cell);
      }
    }
  }
  const keys = [...cells.values()].map((cell) => chunkKey(target, cell));
  await cookCells(target.map, keys);
}

/// Toggles one plant's pin row in the active layer's AnchorOverride chunk for its
/// owner cell — one read-modify-write chunk transaction (a pin protects the plant
/// across graph recooks). Returns the undo/redo pair, or null when the layer takes
/// no edits.
export async function togglePin(
  target: VegetationPaintTarget,
  plant: string,
  cell: { coordinates: [string, string, string]; level: number },
): Promise<{ undo: () => Promise<unknown>; redo: () => Promise<unknown>; pinned: boolean } | null> {
  if (target.locked) {
    return null;
  }
  const key = {
    layer: target.layer,
    tile: { kind: "cell" as const, cell },
    kind: "anchor-override" as const,
  };
  const apply = async (toggleTo: boolean | null): Promise<boolean> => {
    const read = (await client.vegetationMapChunkRead({
      map: target.map,
      keys: [key],
    })) as ChunkReadResult;
    const prior = read.chunks[0] ?? null;
    const payload =
      prior?.payload.kind === "anchor-override"
        ? prior.payload
        : {
            kind: "anchor-override" as const,
            explicitPlants: [],
            pins: [],
            transformOverrides: [],
            stateOverrides: [],
          };
    const pinned = payload.pins.includes(plant);
    const next = toggleTo ?? !pinned;
    if (next === pinned) {
      return pinned;
    }
    await client.vegetationMapChunkCommit({
      map: target.map,
      expectedGeneration: read.generation,
      upserts: [
        {
          key,
          revision: (BigInt(prior?.revision ?? "0") + 1n).toString(),
          payload: {
            ...payload,
            pins: next ? [...payload.pins, plant] : payload.pins.filter((id) => id !== plant),
          },
        },
      ],
      removals: [],
    });
    await cookCells(target.map, [key]);
    return next;
  };
  const pinned = await apply(null);
  return {
    pinned,
    undo: () => apply(!pinned),
    redo: () => apply(pinned),
  };
}

/// Replaces the target layer's tile inside a prior field payload, keeping every other layer's
/// tiles and the payload's other slot untouched. Blocker layers write the `blockers` set, which is
/// the slot the evaluator reads a signed-blocker channel from.
function payloadWithTile(
  target: VegetationPaintTarget,
  prior: { fields: AuthoredFieldTileDto[]; blockers: AuthoredFieldTileDto[] } | null,
  priorTile: AuthoredFieldTileDto | null,
  tile: AuthoredFieldTileDto,
) {
  const fields = prior?.fields ?? [];
  const blockers = prior?.blockers ?? [];
  return target.slot === "blocker"
    ? {
        kind: "field" as const,
        fields,
        blockers: [...blockers.filter((row) => row !== priorTile), tile],
      }
    : {
        kind: "field" as const,
        fields: [...fields.filter((row) => row !== priorTile), tile],
        blockers,
      };
}

/// The tile the target layer already owns in a prior payload, or null when it owns none.
function priorTileOf(
  target: VegetationPaintTarget,
  prior: { fields: AuthoredFieldTileDto[]; blockers: AuthoredFieldTileDto[] } | null,
): AuthoredFieldTileDto | null {
  const rows = target.slot === "blocker" ? (prior?.blockers ?? []) : (prior?.fields ?? []);
  return (
    rows.find(
      (tile) =>
        tile.layer === target.layer &&
        JSON.stringify(tile.channel) === JSON.stringify(target.channel),
    ) ?? null
  );
}

/// Replays a captured payload set over fresh revisions and generation, then recooks what it
/// touched. Both halves of every stroke's undo/redo pair go through this.
function replayChunks(target: VegetationPaintTarget) {
  return async (
    payloads: VegetationMapChunkDto[],
    removals: VegetationMapChunkKeyDto[],
  ): Promise<unknown> => {
    const current = (await client.vegetationMapChunkRead({
      map: target.map,
      keys: [...payloads.map((chunk) => chunk.key), ...removals],
    })) as ChunkReadResult;
    const revisions = new Map(
      current.chunks.map((chunk) => [JSON.stringify(chunk.key), chunk.revision]),
    );
    const touched = [...payloads.map((chunk) => chunk.key), ...removals];
    await client.vegetationMapChunkCommit({
      map: target.map,
      expectedGeneration: current.generation,
      upserts: payloads.map((chunk) => ({
        ...chunk,
        revision: (BigInt(revisions.get(JSON.stringify(chunk.key)) ?? "0") + 1n).toString(),
      })),
      removals: removals.filter((key) =>
        current.chunks.some((chunk) => JSON.stringify(chunk.key) === JSON.stringify(key)),
      ),
    });
    return cookCells(target.map, touched);
  };
}

/// Applies a captured stroke: reads the touched chunks, splats every stamp into
/// per-cell grids seeded from the existing tiles, and commits the replacement
/// chunks in one transaction. Returns the undo/redo pair (each re-reads current
/// revisions at execution time and replays a captured payload set).
export async function commitStroke(
  target: VegetationPaintTarget,
  stamps: BrushStamp[],
): Promise<{ undo: () => Promise<unknown>; redo: () => Promise<unknown> } | null> {
  if (stamps.length === 0 || target.channel === null || target.locked) {
    return null;
  }
  const cells = new Map<string, [bigint, bigint, bigint]>();
  for (const stamp of stamps) {
    // The stamp disk can straddle cell borders: cover the reach of the radius.
    for (const dx of [-stamp.radius, 0, stamp.radius]) {
      for (const dz of [-stamp.radius, 0, stamp.radius]) {
        const cell = chunkCell(
          [stamp.position[0] + dx, stamp.position[1], stamp.position[2] + dz],
          target.chunkLevel,
        );
        cells.set(cellKeyString(cell), cell);
      }
    }
  }
  const keys = [...cells.values()].map((cell) => chunkKey(target, cell));
  const read = (await client.vegetationMapChunkRead({
    map: target.map,
    keys,
  })) as ChunkReadResult;
  const priorByCell = new Map<string, VegetationMapChunkDto>();
  for (const chunk of read.chunks) {
    if (chunk.key.tile.kind === "cell") {
      priorByCell.set(chunk.key.tile.cell.coordinates.join(","), chunk);
    }
  }

  const upserts: VegetationMapChunkDto[] = [];
  const created: VegetationMapChunkKeyDto[] = [];
  for (const cell of cells.values()) {
    const prior = priorByCell.get(cellKeyString(cell));
    const priorPayload = prior?.payload.kind === "field" ? prior.payload : null;
    const priorTile = priorTileOf(target, priorPayload);
    const grid = priorTile ? gridFromTile(priorTile) : freshGrid();
    let touched = false;
    for (const stamp of stamps) {
      touched = splat(grid, cell, target.chunkLevel, stamp) || touched;
    }
    if (!touched) {
      continue;
    }
    const key = chunkKey(target, cell);
    if (!prior) {
      created.push(key);
    }
    upserts.push({
      key,
      revision: (BigInt(prior?.revision ?? "0") + 1n).toString(),
      payload: payloadWithTile(target, priorPayload, priorTile, tileFromGrid(target, grid)),
    });
  }
  if (upserts.length === 0) {
    return null;
  }
  await client.vegetationMapChunkCommit({
    map: target.map,
    expectedGeneration: read.generation,
    upserts,
    removals: [],
  });
  await cookCells(
    target.map,
    upserts.map((chunk) => chunk.key),
  );
  useEditorStore.getState().setVegetationLastStroke(strokeBounds(stamps));

  const priorChunks = [...priorByCell.values()];
  const strokeChunks = upserts.map((chunk) => ({ ...chunk }));
  const replay = replayChunks(target);
  return {
    undo: () => replay(priorChunks, created),
    redo: () => replay(strokeChunks, []),
  };
}

/// Lays `level` across the whole authored tile of the chunk cell holding `position` — the Fill
/// gesture. One cell, one transaction, one recook: fill is bounded by the cell it lands in rather
/// than flooding the map, so a click can never queue an unbounded cook.
export async function commitFill(
  target: VegetationPaintTarget,
  position: [number, number, number],
  level: number,
): Promise<{ undo: () => Promise<unknown>; redo: () => Promise<unknown> } | null> {
  if (target.channel === null || target.locked) {
    return null;
  }
  const cell = chunkCell(position, target.chunkLevel);
  const key = chunkKey(target, cell);
  const read = (await client.vegetationMapChunkRead({
    map: target.map,
    keys: [key],
  })) as ChunkReadResult;
  const prior = read.chunks[0] ?? null;
  const priorPayload = prior?.payload.kind === "field" ? prior.payload : null;
  const priorTile = priorTileOf(target, priorPayload);
  const grid = priorTile ? gridFromTile(priorTile) : freshGrid();
  const clamped = Math.min(1, Math.max(0, level));
  if (grid.densities.every((value) => value === clamped)) {
    return null;
  }
  grid.densities.fill(clamped);
  const filled: VegetationMapChunkDto = {
    key,
    revision: (BigInt(prior?.revision ?? "0") + 1n).toString(),
    payload: payloadWithTile(target, priorPayload, priorTile, tileFromGrid(target, grid)),
  };
  await client.vegetationMapChunkCommit({
    map: target.map,
    expectedGeneration: read.generation,
    upserts: [filled],
    removals: [],
  });
  await cookCells(target.map, [key]);
  const replay = replayChunks(target);
  return {
    undo: () => replay(prior ? [prior] : [], prior ? [] : [key]),
    redo: () => replay([filled], []),
  };
}
