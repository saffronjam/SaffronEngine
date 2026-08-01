/// Which gesture each vegetation tool begins, and which tools consume a click. A tool whose value
/// nothing reads goes inert without any visible symptom — the toolbar still highlights it — so the
/// whole vocabulary is asserted here rather than only the tools a given change touched.
import { beforeEach, describe, expect, mock, test } from "bun:test";

// The gestures commit through the control plane on release; what is under test is the decision a
// press makes, so every call resolves to a pick that found no ground unless a case sets `picked`.
// The call names are recorded because consuming a click and reaching the command the tool names are
// two different things, and a handler that dispatched to nothing would satisfy only the first.
const calls: string[] = [];
let picked: Record<string, unknown> = {};
mock.module("../../control/client", () => ({
  client: new Proxy(
    {},
    {
      get: (_target, name: string) => async () => {
        calls.push(name);
        return name === "pick" ? picked : {};
      },
    },
  ),
}));

import { defaultSceneLayout, insertPanel } from "../../state/dockLayout";
import { useEditorStore, type VegetationPaintTarget } from "../../state/store";
import { VEGETATION_TOOLS } from "../vegetationTools";
import { beginVegetationPress, clickVegetationTool } from "./vegetationPress";

// A layout write wakes the store's layout-settled probe on the next frame, and that probe measures
// panel rects. There is no document here for it to measure, so the frame never arrives.
globalThis.requestAnimationFrame = () => 0;

const UV = { u: 0.5, v: 0.5 };

/// A paintable layer: a scalar field the density and paint families write.
function fieldLayer(patch: Partial<VegetationPaintTarget> = {}): VegetationPaintTarget {
  return {
    map: "map",
    layer: "layer",
    operator: "scatter",
    channel: "density",
    slot: "field",
    chunkLevel: 0,
    locked: false,
    ...patch,
  };
}

function openVegetationPanel(): void {
  useEditorStore.setState({
    dockLayouts: {
      ...useEditorStore.getState().dockLayouts,
      scene: insertPanel(defaultSceneLayout(), "vegetation", "leaf:bottom", 0),
    },
  });
}

function closeVegetationPanel(): void {
  useEditorStore.setState({
    dockLayouts: { ...useEditorStore.getState().dockLayouts, scene: defaultSceneLayout() },
  });
}

/// The gesture kind a press with `tool` begins against the given active layer, or null.
function pressKind(
  tool: (typeof VEGETATION_TOOLS)[number]["tool"],
  target: VegetationPaintTarget | null,
): string | null {
  useEditorStore.setState({ vegetationTool: tool, vegetationActiveLayer: target });
  return beginVegetationPress(UV, 1)?.kind ?? null;
}

function clicked(tool: (typeof VEGETATION_TOOLS)[number]["tool"]): boolean {
  useEditorStore.setState({ vegetationTool: tool });
  return clickVegetationTool(UV);
}

beforeEach(() => {
  openVegetationPanel();
  calls.length = 0;
  picked = {};
  useEditorStore.setState({
    playState: "edit",
    vegetationActiveLayer: fieldLayer(),
    vegetationSpecies: new Set(["family"]),
  });
});

describe("the vegetation tool vocabulary reaches a gesture", () => {
  test("every declared tool either begins a press gesture or consumes a click", () => {
    const inert = VEGETATION_TOOLS.filter(
      ({ tool }) =>
        pressKind(tool, tool === "exclude" ? fieldLayer({ slot: "blocker" }) : fieldLayer()) ===
          null && !clicked(tool),
    ).map(({ tool }) => tool);
    // Select is the one tool with no gesture of its own: it falls through to the viewport's
    // ordinary ray-pick selection.
    expect(inert).toEqual(["select"]);
  });

  test("each tool begins the gesture its palette entry names", () => {
    expect(pressKind("select", fieldLayer())).toBeNull();
    expect(pressKind("lasso", fieldLayer())).toBe("lasso");
    expect(pressKind("volume", fieldLayer())).toBe("volume");
    for (const tool of ["paint", "erase", "density", "reapply"] as const) {
      expect(pressKind(tool, fieldLayer())).toBe("stroke");
    }
    expect(pressKind("exclude", fieldLayer({ slot: "blocker" }))).toBe("stroke");
    // The click tools draw no drag gesture.
    for (const tool of ["single", "fill", "spline", "pin", "promote"] as const) {
      expect(pressKind(tool, fieldLayer())).toBeNull();
    }
  });

  test("each click tool consumes the click and no other tool does", () => {
    const consuming = VEGETATION_TOOLS.filter(({ tool }) => clicked(tool)).map(({ tool }) => tool);
    expect(consuming).toEqual(["single", "fill", "spline", "pin", "promote"]);
  });

  test("Promote carries the plant under the cursor to the engine's promotion command", async () => {
    picked = { plant: "77" };
    expect(clicked("promote")).toBe(true);
    // The click starts the gesture and does not await it; a macrotask drains the two round-trips.
    await new Promise((resolve) => setTimeout(resolve, 0));
    expect(calls).toEqual(["pick", "vegetationPromote"]);
  });

  test("Single needs a species to plant, so it declines the click without one", () => {
    useEditorStore.setState({ vegetationSpecies: new Set<string>() });
    expect(clicked("single")).toBe(false);
    useEditorStore.setState({ vegetationSpecies: new Set(["family"]) });
    expect(clicked("single")).toBe(true);
  });
});

describe("a stroke tool aimed at the wrong layer writes nothing", () => {
  test("Exclude writes the blocker slot and the field tools write the field slot", () => {
    expect(pressKind("exclude", fieldLayer())).toBeNull();
    expect(pressKind("paint", fieldLayer({ slot: "blocker" }))).toBeNull();
    expect(pressKind("density", fieldLayer({ slot: "blocker" }))).toBeNull();
  });

  test("a locked layer and a layer with no channel take no stroke", () => {
    expect(pressKind("paint", fieldLayer({ locked: true }))).toBeNull();
    expect(pressKind("paint", fieldLayer({ channel: null }))).toBeNull();
    // Reapply recooks rather than writing tiles, so neither stops it.
    expect(pressKind("reapply", fieldLayer({ locked: true, channel: null }))).toBe("stroke");
  });

  test("with no active layer only the layer-free gestures run", () => {
    expect(pressKind("paint", null)).toBeNull();
    expect(pressKind("reapply", null)).toBeNull();
    expect(pressKind("lasso", null)).toBe("lasso");
    expect(pressKind("volume", null)).toBe("volume");
  });
});

describe("when a gesture may run at all", () => {
  test("with the panel closed no tool touches the viewport", () => {
    closeVegetationPanel();
    for (const { tool } of VEGETATION_TOOLS) {
      expect(pressKind(tool, fieldLayer({ slot: "blocker" }))).toBeNull();
      expect(clicked(tool)).toBe(false);
    }
  });

  test("while the world plays only the tools that author nothing stay live", () => {
    useEditorStore.setState({ playState: "playing" });
    expect(pressKind("lasso", fieldLayer())).toBe("lasso");
    expect(clicked("promote")).toBe(true);
    expect(pressKind("paint", fieldLayer())).toBeNull();
    expect(pressKind("volume", fieldLayer())).toBeNull();
    expect(clicked("single")).toBe(false);
    expect(clicked("fill")).toBe(false);
  });
});
