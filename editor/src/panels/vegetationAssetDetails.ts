import type {
  VegetationManifestDependencyAddressDto,
  WorldBoundsDto,
  WorldCellDto,
} from "../protocol/sa-types";
import { humanizeFieldName } from "../lib/humanize";

export interface DependencyDescription {
  kind: string;
  subject: string;
}

export function formatVegetationCount(value: string): string {
  const parsed = parseUnsigned(value);
  return parsed === null ? value : parsed.toLocaleString();
}

export function formatVegetationBytes(value: string): string {
  const bytes = parseUnsigned(value);
  if (bytes === null) {
    return value;
  }
  if (bytes < 1024n) {
    return `${bytes.toLocaleString()} B`;
  }

  const units = ["KiB", "MiB", "GiB", "TiB", "PiB"];
  let divisor = 1024n;
  let unit = 0;
  while (unit < units.length - 1 && bytes >= divisor * 1024n) {
    divisor *= 1024n;
    unit += 1;
  }
  return `${formatRatio(bytes, divisor)} ${units[unit]}`;
}

export function formatVegetationDuration(value: string): string {
  const micros = parseUnsigned(value);
  if (micros === null) {
    return value;
  }
  if (micros < 1_000n) {
    return `${micros.toLocaleString()} µs`;
  }
  if (micros < 1_000_000n) {
    return `${formatRatio(micros, 1_000n)} ms`;
  }
  return `${formatRatio(micros, 1_000_000n)} s`;
}

export function formatVegetationCacheRate(hits: string, misses: string): string {
  const parsedHits = parseUnsigned(hits);
  const parsedMisses = parseUnsigned(misses);
  if (parsedHits === null || parsedMisses === null || parsedHits + parsedMisses === 0n) {
    return "—";
  }
  const tenths =
    (parsedHits * 1_000n + (parsedHits + parsedMisses) / 2n) / (parsedHits + parsedMisses);
  return `${tenths / 10n}.${tenths % 10n}%`;
}

export function describeVegetationDependency(
  address: VegetationManifestDependencyAddressDto,
  assetNames: ReadonlyMap<string, string>,
): DependencyDescription {
  switch (address.kind) {
    case "source-asset":
      return { kind: "Source asset", subject: assetLabel(address.asset, assetNames) };
    case "source-file":
      return { kind: "Source file", subject: address.uri };
    case "material-coverage":
      return { kind: "Material coverage", subject: assetLabel(address.material, assetNames) };
    case "biome-ir":
      return {
        kind: "Biome IR",
        subject: `${assetLabel(address.map, assetNames)} · instance ${address.instance}`,
      };
    case "map-manifest":
      return { kind: "Map manifest", subject: assetLabel(address.map, assetNames) };
    case "map-object":
      return {
        kind: "Map object",
        subject: `${assetLabel(address.map, assetNames)} · ${humanizeFieldName(address.key.kind)} · layer ${address.key.layer} · ${address.key.tile.kind === "global" ? "Global" : formatWorldCell(address.key.tile.cell)}`,
      };
    case "surface-provider":
      return {
        kind: "Surface provider",
        subject: `${address.provider} · revision ${address.revision}`,
      };
    case "surface-tile":
      return {
        kind: "Surface tile",
        subject: `${address.provider} · revision ${address.revision} · ${address.channel === null ? "All channels" : humanizeFieldName(address.channel.kind)} · ${formatWorldBounds(address.bounds)}`,
      };
    case "contract":
      return { kind: "Contract", subject: address.namespace };
    case "node":
      if (address.node.kind === "plant") {
        return {
          kind: "Cook node",
          subject: `Plant · ${assetLabel(address.node.family, assetNames)}`,
        };
      }
      if (address.node.kind === "global-stage") {
        return {
          kind: "Cook node",
          subject: `Global stage · ${address.node.stage} · ${formatWorldCell(address.node.owner)}`,
        };
      }
      return { kind: "Cook node", subject: `Cell · ${formatWorldCell(address.node.cell)}` };
  }
}

export function formatWorldCell(cell: WorldCellDto): string {
  return `L${cell.level} (${cell.coordinates.join(", ")})`;
}

export function formatWorldBounds(bounds: WorldBoundsDto): string {
  return `${bounds.minTicks.join(", ")} → ${bounds.maxTicksExclusive.join(", ")}`;
}

function parseUnsigned(value: string): bigint | null {
  if (!/^\d+$/.test(value)) {
    return null;
  }
  return BigInt(value);
}

function formatRatio(value: bigint, divisor: bigint): string {
  const tenths = (value * 10n + divisor / 2n) / divisor;
  const whole = tenths / 10n;
  const fraction = tenths % 10n;
  return fraction === 0n ? whole.toLocaleString() : `${whole.toLocaleString()}.${fraction}`;
}

function assetLabel(asset: string, assetNames: ReadonlyMap<string, string>): string {
  const name = assetNames.get(asset);
  return name === undefined ? asset : `${name} (${asset})`;
}
