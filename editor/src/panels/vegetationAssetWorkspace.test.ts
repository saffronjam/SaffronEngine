import { describe, expect, test } from "bun:test";
import {
  describeVegetationDependency,
  formatVegetationBytes,
  formatVegetationCacheRate,
  formatVegetationCount,
  formatVegetationDuration,
  formatWorldBounds,
  formatWorldCell,
} from "./vegetationAssetDetails";

const ASSETS = new Map([
  ["00000000-0000-0000-0000-000000000001", "Oak"],
  ["00000000-0000-0000-0000-000000000002", "Forest"],
]);

describe("vegetation asset workspace formatting", () => {
  test("formats wire-sized counters without losing integer precision", () => {
    expect(formatVegetationCount("18446744073709551615")).toBe(
      18446744073709551615n.toLocaleString(),
    );
    expect(formatVegetationBytes("1536")).toBe("1.5 KiB");
    expect(formatVegetationDuration("1500")).toBe("1.5 ms");
    expect(formatVegetationDuration("2500000")).toBe("2.5 s");
  });

  test("formats cache rates from exact wire counters", () => {
    expect(formatVegetationCacheRate("3", "1")).toBe("75.0%");
    expect(formatVegetationCacheRate("0", "0")).toBe("—");
  });

  test("keeps malformed diagnostic values visible", () => {
    expect(formatVegetationCount("unknown")).toBe("unknown");
    expect(formatVegetationBytes("unknown")).toBe("unknown");
    expect(formatVegetationDuration("unknown")).toBe("unknown");
  });

  test("describes catalog and spatial dependency addresses", () => {
    expect(
      describeVegetationDependency(
        { kind: "source-asset", asset: "00000000-0000-0000-0000-000000000001" },
        ASSETS,
      ),
    ).toEqual({
      kind: "Source asset",
      subject: "Oak (00000000-0000-0000-0000-000000000001)",
    });
    expect(
      describeVegetationDependency(
        {
          kind: "surface-tile",
          provider: "terrain",
          revision: "42",
          channel: { kind: "slope" },
          bounds: { minTicks: ["-4", "0", "8"], maxTicksExclusive: ["4", "2", "16"] },
        },
        ASSETS,
      ),
    ).toEqual({
      kind: "Surface tile",
      subject: "terrain · revision 42 · Slope · -4, 0, 8 → 4, 2, 16",
    });
  });

  test("formats cell keys and half-open world bounds", () => {
    expect(formatWorldCell({ coordinates: ["-2", "3", "0"], level: 4 })).toBe("L4 (-2, 3, 0)");
    expect(
      formatWorldBounds({ minTicks: ["-8", "0", "1"], maxTicksExclusive: ["8", "16", "9"] }),
    ).toBe("-8, 0, 1 → 8, 16, 9");
  });
});
