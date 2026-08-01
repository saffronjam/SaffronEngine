/// Anchor-planting synthesis: turns a picked world position and a selected species
/// into one typed `AnchorAddition` mutation record, plus the shared mutation-header
/// vocabulary (fresh per-call transaction/idempotency GUIDs, the editor authority).
/// Anchors mint explicit-namespace plant ids; the reducer rejects any other namespace.
import type { VegetationMutationDto, VegetationMutationRecordDto, WorldCellDto } from "../protocol";

/// The editor's stable mutation authority id.
export const EDITOR_AUTHORITY = "0000000000000000000000000000e017";

/// Cell-local ticks per metre (1/4096 m) and level-zero cell edge ticks.
const TICKS_PER_METER = 4096n;
const CELL_TICKS = 64n * TICKS_PER_METER;

/// A fresh 32-hex vegetation GUID (transaction/idempotency keys).
export function freshGuid(): string {
  return crypto.randomUUID().replaceAll("-", "");
}

/// A fresh explicit-namespace PlantId: random payload with the high two bits of the
/// first byte forced to `01` (the authored/explicit namespace tag).
export function freshExplicitPlantId(): string {
  const hex = freshGuid();
  const first = Number.parseInt(hex.slice(0, 2), 16);
  const tagged = (first & 0x3f) | 0x40;
  return tagged.toString(16).padStart(2, "0") + hex.slice(2);
}

/// One mutation record with fresh ids over the shared editor header.
export function mutationRecord(
  cell: WorldCellDto,
  mutation: VegetationMutationDto,
): VegetationMutationRecordDto {
  return {
    header: {
      cell,
      transaction: freshGuid(),
      authority: EDITOR_AUTHORITY,
      logicalTick: String(Date.now()),
      idempotencyKey: freshGuid(),
    },
    mutation,
  };
}

function floorDiv(value: bigint, divisor: bigint): bigint {
  const quotient = value / divisor;
  return value % divisor !== 0n && value < 0n !== divisor < 0n ? quotient - 1n : quotient;
}

/// The level-zero cell + local ticks for a world position in metres.
export function worldToCell(position: [number, number, number]): {
  cell: WorldCellDto;
  localTicks: [number, number, number];
  globalTicks: [bigint, bigint, bigint];
} {
  const global = position.map((meters) => BigInt(Math.round(meters * 4096))) as [
    bigint,
    bigint,
    bigint,
  ];
  const cellCoordinates = global.map((ticks) => floorDiv(ticks, CELL_TICKS)) as [
    bigint,
    bigint,
    bigint,
  ];
  const local = global.map((ticks, axis) => ticks - cellCoordinates[axis]! * CELL_TICKS) as [
    bigint,
    bigint,
    bigint,
  ];
  return {
    cell: {
      coordinates: [
        cellCoordinates[0].toString(),
        cellCoordinates[1].toString(),
        cellCoordinates[2].toString(),
      ],
      level: 0,
    },
    localTicks: [Number(local[0]), Number(local[1]), Number(local[2])],
    globalTicks: global,
  };
}

/// Builds the anchor record for one planted species at a picked ground position.
/// Bounds are a conservative editor box around the point (the render path composes
/// the family's real prototype bounds regardless).
export function anchorRecord(
  position: [number, number, number],
  family: string,
): VegetationMutationRecordDto {
  const { cell, localTicks, globalTicks } = worldToCell(position);
  const plant = freshExplicitPlantId();
  const horizontal = 8n * TICKS_PER_METER;
  const up = 16n * TICKS_PER_METER;
  const down = 1n * TICKS_PER_METER;
  const mutation: VegetationMutationDto = {
    kind: "anchor-addition",
    point: {
      id: plant,
      owner: cell,
      localPosition: localTicks,
      orientation: [0, 0, 0, 32767],
      scaleBits: [65536, 65536, 65536],
      bounds: {
        minTicks: [
          (globalTicks[0] - horizontal).toString(),
          (globalTicks[1] - down).toString(),
          (globalTicks[2] - horizontal).toString(),
        ],
        maxTicksExclusive: [
          (globalTicks[0] + horizontal).toString(),
          (globalTicks[1] + up).toString(),
          (globalTicks[2] + horizontal).toString(),
        ],
      },
      family,
      variation: 0,
      lifecycle: "mature",
      phenotype: 0,
      representationClass: 0,
      deterministicKey: freshGuid(),
      candidate: "0",
      ecologyTick: "1",
      health: 65535,
      moisture: 32768,
      fuel: 32768,
      phenology: 0,
      flags: 1,
      interactionPolicy: "interactive",
      provenance: 0,
      surfaceProjectionBits: [0, 0, 0],
    },
  };
  return mutationRecord(cell, mutation);
}
