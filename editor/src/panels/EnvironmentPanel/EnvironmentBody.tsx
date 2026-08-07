import { useState } from "react";
import { client } from "../../control/client";
import { errorText, notifyError } from "../../lib/flash";
import { useEditorStore } from "../../state/store";
import { ScrollArea } from "@/components/ui/scroll-area";
import type { Environment } from "../../protocol";
import { CurvesDialog } from "./CurvesDialog";
import type { EnvironmentSectionContext } from "./sectionContext";
import { ClockSection } from "./sections/ClockSection";
import { FogSection } from "./sections/FogSection";
import { AtmosphereSection, BackgroundSection, LightingSection } from "./sections/SkySections";
import { CloudsSection, WindSection } from "./sections/WeatherSections";
import { useEnvironmentEditor, type NestedBlock } from "./useEnvironmentEditor";
import type { EnvironmentSection, EnvironmentViewState } from "./viewState";

/// Which section a keyword group belongs to, and the words a search matches against.
const GROUPS: { id: string; section: EnvironmentSection; keywords: string }[] = [
  {
    id: "background",
    section: "sky",
    keywords: "sky background mode color texture procedural intensity rotation visible",
  },
  {
    id: "lighting",
    section: "sky",
    keywords: "environment lighting fallback ambient source color intensity directional sky",
  },
  {
    id: "clock",
    section: "time",
    keywords:
      "time of day clock playback sun manual date year month day latitude longitude location automation curves exposure tint coverage cloud type",
  },
  {
    id: "atmosphere",
    section: "sky",
    keywords:
      "physical atmosphere rayleigh mie ozone planet radius height sun moon celestial earthshine",
  },
  {
    id: "clouds",
    section: "weather",
    keywords:
      "clouds coverage type precipitation anvil layer altitude height shape noise detail curl weather map droplet shadows",
  },
  { id: "wind", section: "weather", keywords: "wind direction orientation speed gust advection" },
  {
    id: "fogAppearance",
    section: "fog",
    keywords:
      "fog mode density albedo height falloff distance opacity volumetric medium scatter phase lighting emissive sun ground haze aerial perspective",
  },
];

/// The scrolling body: every section that the active tab (or the live search) admits, plus the
/// appearance-curves dialog. Split from the panel shell so the editor hooks always see a
/// non-null environment.
export function EnvironmentBody({
  env,
  defaults,
  query,
  viewState,
  setViewState,
  onCustomized,
}: {
  env: Environment;
  defaults: Environment | null;
  query: string;
  viewState: EnvironmentViewState;
  setViewState: React.Dispatch<React.SetStateAction<EnvironmentViewState>>;
  onCustomized(): void;
}) {
  const setEnvironment = useEditorStore((s) => s.setEnvironment);
  const editor = useEnvironmentEditor(env, onCustomized);
  const [curvesOpen, setCurvesOpen] = useState(false);

  const visible = (id: string): boolean => {
    const group = GROUPS.find((entry) => entry.id === id);
    if (!group) {
      return false;
    }
    return query.length === 0
      ? viewState.section === group.section
      : group.keywords.includes(query);
  };

  /// One replace-environment write plus its undo entry, used by every section reset. A rejection
  /// restores the prior snapshot locally and surfaces the reason.
  const replaceSnapshot = (label: string, next: Environment): void => {
    const prior = structuredClone(env);
    if (JSON.stringify(prior) === JSON.stringify(next)) {
      return;
    }
    onCustomized();
    setEnvironment(next);
    useEditorStore.getState().pushEdit(
      {
        label,
        undo: () => client.replaceEnvironment(prior),
        redo: () => client.replaceEnvironment(next),
      },
      "scene",
    );
    void client
      .replaceEnvironment(next)
      .then(setEnvironment)
      .catch((error: unknown) => {
        setEnvironment(prior);
        notifyError(errorText(error));
      });
  };

  const ctx: EnvironmentSectionContext = {
    env,
    defaults,
    allDetails: viewState.detail === "all" || query.length > 0,
    editor,
    sectionOpen: (id) => query.length > 0 || viewState.expanded[id] === true,
    setSectionOpen: (id, open) =>
      setViewState((current) => ({
        ...current,
        expanded: { ...current.expanded, [id]: open },
      })),
    isModified: (current, initial) =>
      initial !== undefined && JSON.stringify(current) !== JSON.stringify(initial),
    resetTopLevel: (label, fields) => {
      if (!defaults) {
        return;
      }
      const next = structuredClone(env);
      for (const field of fields) {
        (next as unknown as Record<string, unknown>)[field] = structuredClone(defaults[field]);
      }
      replaceSnapshot(label, next);
    },
    resetNested: <B extends NestedBlock>(
      label: string,
      block: B,
      fields?: (keyof Environment[B])[],
    ) => {
      if (!defaults) {
        return;
      }
      const next = structuredClone(env);
      if (!fields) {
        next[block] = structuredClone(defaults[block]) as Environment[B];
      } else {
        const target = next[block] as unknown as Record<string, unknown>;
        for (const field of fields) {
          target[field as string] = structuredClone(defaults[block][field]);
        }
      }
      replaceSnapshot(label, next);
    },
  };

  return (
    <>
      <ScrollArea className="min-h-0 flex-1">
        <div className="flex flex-col gap-2 p-2.5">
          {visible("background") ? <BackgroundSection ctx={ctx} /> : null}
          {visible("lighting") ? <LightingSection ctx={ctx} /> : null}
          {visible("clock") ? (
            <ClockSection ctx={ctx} onOpenCurves={() => setCurvesOpen(true)} />
          ) : null}
          {visible("atmosphere") ? <AtmosphereSection ctx={ctx} /> : null}
          {visible("clouds") ? <CloudsSection ctx={ctx} /> : null}
          {visible("wind") ? <WindSection ctx={ctx} /> : null}
          {visible("fogAppearance") ? <FogSection ctx={ctx} /> : null}
        </div>
      </ScrollArea>
      <CurvesDialog
        open={curvesOpen}
        onOpenChange={setCurvesOpen}
        tod={env.timeOfDay}
        editor={editor}
      />
    </>
  );
}
