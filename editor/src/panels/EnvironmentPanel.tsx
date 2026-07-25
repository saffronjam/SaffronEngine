/// The Environment panel owns the rendered world: sky, time, weather, and fog. Progressive
/// disclosure keeps the common controls visible while preserving access to the complete model;
/// renderer-cost controls live in Render and image-formation controls live in Post.
///
/// Units (the 57x bug guard): skyRotation is RADIANS on the wire but shown in
/// DEGREES in the UI — conversion happens ONLY at the rotation widget boundary
/// here. Exposure is deliberately NOT here: `SceneEnvironment.exposure` is reserved
/// on the wire; the effective tonemap exposure is the render-side `set-exposure`,
/// surfaced in Post.
///
/// `set-environment` is a server-side MERGE over the current environment, so every
/// write sends only the one named field that changed (a `Partial<Environment>`).
/// High-frequency edits (drags/sliders) funnel through per-field coalescers and the
/// drag bracket flips `store.dragActive` so the reconcile poll won't clobber the
/// optimistic value mid-scrub.
import { useEffect, useMemo, useRef, useState } from "react";
import { Search, Save } from "lucide-react";
import { client } from "../control/client";
import { useEditorStore } from "../state/store";
import { makeCoalescer, type Coalescer } from "../control/coalesce";
import { NumberDrag } from "../components/NumberDrag";
import { ColorField } from "../components/ColorField";
import { VectorEditor } from "../components/VectorEditor";
import { AssetPicker } from "../components/AssetPicker";
import { PropertySection } from "../components/PropertySection";
import {
  IDENTITY_CURVE,
  ToneCurve,
  type CurvePoint,
  type ToneCurveChannels,
} from "../components/ToneCurve";
import type { Environment, EnvironmentProfileSummaryDto, Vec3 } from "../protocol";
import { DEG_TO_RAD, RAD_TO_DEG } from "@/lib/utils";
import { humanizeFieldName } from "@/lib/humanize";
import { errorText, notify, notifyError } from "../lib/flash";
import { Label } from "@/components/ui/label";
import { ScrollArea } from "@/components/ui/scroll-area";
import { Switch } from "@/components/ui/switch";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Tabs, TabsList, TabsTrigger } from "@/components/ui/tabs";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";

type SkyMode = Environment["skyMode"];
type Atmosphere = Environment["atmosphere"];
type Fog = Environment["fog"];
type Cloud = Environment["cloud"];
type Wind = Environment["wind"];
type TimeOfDay = Environment["timeOfDay"];
type EnvironmentSection = "sky" | "time" | "weather" | "fog";
type DetailLevel = "essential" | "all";

const VIEW_STATE_KEY = "saffron.environment-panel.v1";

interface EnvironmentViewState {
  section: EnvironmentSection;
  detail: DetailLevel;
  expanded: Record<string, boolean>;
}

const DEFAULT_VIEW_STATE: EnvironmentViewState = {
  section: "sky",
  detail: "essential",
  expanded: {
    background: true,
    lighting: true,
    atmosphere: false,
    clock: true,
    location: false,
    automation: false,
    clouds: true,
    wind: false,
    fogAppearance: true,
    fogLighting: false,
    groundHaze: false,
    aerialPerspective: false,
  },
};

function loadViewState(): EnvironmentViewState {
  try {
    const saved = JSON.parse(
      localStorage.getItem(VIEW_STATE_KEY) ?? "null",
    ) as Partial<EnvironmentViewState> | null;
    return {
      section: saved?.section ?? DEFAULT_VIEW_STATE.section,
      detail: saved?.detail ?? DEFAULT_VIEW_STATE.detail,
      expanded: { ...DEFAULT_VIEW_STATE.expanded, ...saved?.expanded },
    };
  } catch {
    return DEFAULT_VIEW_STATE;
  }
}

function profileKey(profile: EnvironmentProfileSummaryDto["reference"]): string {
  return profile.kind === "asset" ? `asset:${profile.id}` : `builtin:${profile.profile}`;
}

const emptyScalarChannels = (curve: CurvePoint[]): ToneCurveChannels => ({
  master: curve,
  r: [],
  g: [],
  b: [],
});

const tintChannels = (tint: TimeOfDay["tintCurve"]): ToneCurveChannels => ({
  master: tint.master,
  r: tint.red,
  g: tint.green,
  b: tint.blue,
});

const timeOfDayTint = (channels: ToneCurveChannels): TimeOfDay["tintCurve"] => ({
  master: channels.master,
  red: channels.r,
  green: channels.g,
  blue: channels.b,
});

const identityCurve = (): CurvePoint[] => IDENTITY_CURVE.map((point) => ({ ...point }));

const SKY_MODES: { value: SkyMode; label: string }[] = [
  { value: "color", label: "Color" },
  { value: "texture", label: "Texture" },
  { value: "procedural", label: "Procedural" },
];

const FOG_MODES: { value: Fog["mode"]; label: string }[] = [
  { value: "analytic", label: "Analytic" },
  { value: "volumetric", label: "Volumetric" },
];

/// A labelled row: a left caption + the widget, matching the inspector's grid.
function Row({ label, children }: { label: string; children: React.ReactNode }) {
  return (
    <div className="grid grid-cols-[96px_1fr] items-center gap-1.5">
      <Label className="truncate text-[11px] font-normal text-muted-foreground">{label}</Label>
      <div className="min-w-0">{children}</div>
    </div>
  );
}

export function EnvironmentPanel() {
  const phase = useEditorStore((s) => s.engineStatus.phase);
  const sceneVersion = useEditorStore((s) => s.sceneVersion);
  const environment = useEditorStore((s) => s.environment);
  const setEnvironment = useEditorStore((s) => s.setEnvironment);
  const setDragActive = useEditorStore((s) => s.setDragActive);

  const ready = phase === "ready";
  const [viewState, setViewState] = useState(loadViewState);
  const [search, setSearch] = useState("");
  const [defaults, setDefaults] = useState<Environment | null>(null);
  const [profiles, setProfiles] = useState<EnvironmentProfileSummaryDto[]>([]);
  const [activeProfileKey, setActiveProfileKey] = useState("custom");
  const [saveOpen, setSaveOpen] = useState(false);
  const [saveName, setSaveName] = useState("");
  const [curvesOpen, setCurvesOpen] = useState(false);

  useEffect(() => {
    localStorage.setItem(VIEW_STATE_KEY, JSON.stringify(viewState));
  }, [viewState]);

  // Fetch on mount and whenever the scene/project changes (a load swaps the env).
  // The reconcile poll also refreshes it on a scene change; this guarantees the
  // panel is correct even if the poll's gate (focus/drag) skipped that tick.
  useEffect(() => {
    if (!ready) {
      return;
    }
    let cancelled = false;
    void client
      .getEnvironment()
      .then((env) => {
        if (!cancelled && !useEditorStore.getState().dragActive) {
          useEditorStore.getState().setEnvironment(env);
        }
      })
      .catch(() => {});
    return () => {
      cancelled = true;
    };
  }, [ready, sceneVersion]);

  useEffect(() => {
    if (!ready) return;
    let cancelled = false;
    void Promise.all([client.getEnvironmentDefaults(), client.listEnvironmentProfiles()])
      .then(([nextDefaults, list]) => {
        if (!cancelled) {
          setDefaults(nextDefaults);
          setProfiles(list.profiles);
        }
      })
      .catch(() => {});
    return () => {
      cancelled = true;
    };
  }, [ready, sceneVersion]);

  // Per-field coalescers, rebuilt when the field set is stable. The send pushes the
  // single named field through `set-environment` (server merges) and folds the
  // merged result back into the store so a clamp/normalize round-trips.
  const coalescers = useRef(new Map<keyof Environment, Coalescer<Partial<Environment>>>());
  const coalescerFor = useMemo(
    () =>
      (field: keyof Environment): Coalescer<Partial<Environment>> => {
        let c = coalescers.current.get(field);
        if (!c) {
          c = makeCoalescer<Partial<Environment>>({
            send: async (patch) => {
              const merged = await client.setEnvironment(patch);
              if (!useEditorStore.getState().dragActive) {
                useEditorStore.getState().setEnvironment(merged);
              }
            },
          });
          coalescers.current.set(field, c);
        }
        return c;
      },
    [],
  );

  // Atmosphere fields route through `set-atmosphere` (a server-side merge over the
  // current atmosphere block) rather than `set-environment`, since SetEnvironmentParams
  // carries no atmosphere field. The merged environment is folded back like above.
  const atmosCoalescers = useRef(new Map<keyof Atmosphere, Coalescer<Partial<Atmosphere>>>());
  const atmosCoalescerFor = useMemo(
    () =>
      (field: keyof Atmosphere): Coalescer<Partial<Atmosphere>> => {
        let c = atmosCoalescers.current.get(field);
        if (!c) {
          c = makeCoalescer<Partial<Atmosphere>>({
            send: async (patch) => {
              const merged = await client.setAtmosphere(patch);
              if (!useEditorStore.getState().dragActive) {
                useEditorStore.getState().setEnvironment(merged);
              }
            },
          });
          atmosCoalescers.current.set(field, c);
        }
        return c;
      },
    [],
  );

  // Fog fields route through `set-fog` (a server-side merge over the current `fog` block)
  // rather than `set-environment`, for the same reason as atmosphere. The merged
  // environment is folded back like above.
  const fogCoalescers = useRef(new Map<keyof Fog, Coalescer<Partial<Fog>>>());
  const fogCoalescerFor = useMemo(
    () =>
      (field: keyof Fog): Coalescer<Partial<Fog>> => {
        let c = fogCoalescers.current.get(field);
        if (!c) {
          c = makeCoalescer<Partial<Fog>>({
            send: async (patch) => {
              const merged = await client.setFog(patch);
              if (!useEditorStore.getState().dragActive) {
                useEditorStore.getState().setEnvironment(merged);
              }
            },
          });
          fogCoalescers.current.set(field, c);
        }
        return c;
      },
    [],
  );

  const cloudCoalescers = useRef(new Map<keyof Cloud, Coalescer<Partial<Cloud>>>());
  const cloudCoalescerFor = useMemo(
    () =>
      (field: keyof Cloud): Coalescer<Partial<Cloud>> => {
        let c = cloudCoalescers.current.get(field);
        if (!c) {
          c = makeCoalescer<Partial<Cloud>>({
            send: async (patch) => {
              const merged = await client.setClouds(patch);
              if (!useEditorStore.getState().dragActive) {
                useEditorStore.getState().setEnvironment(merged);
              }
            },
          });
          cloudCoalescers.current.set(field, c);
        }
        return c;
      },
    [],
  );

  const windCoalescers = useRef(new Map<keyof Wind, Coalescer<Partial<Wind>>>());
  const windCoalescerFor = useMemo(
    () =>
      (field: keyof Wind): Coalescer<Partial<Wind>> => {
        let c = windCoalescers.current.get(field);
        if (!c) {
          c = makeCoalescer<Partial<Wind>>({
            send: async (patch) => {
              const merged = await client.setWind(patch);
              if (!useEditorStore.getState().dragActive) {
                useEditorStore.getState().setEnvironment(merged);
              }
            },
          });
          windCoalescers.current.set(field, c);
        }
        return c;
      },
    [],
  );

  const todCoalescers = useRef(new Map<keyof TimeOfDay, Coalescer<Partial<TimeOfDay>>>());
  const todCoalescerFor = useMemo(
    () =>
      (field: keyof TimeOfDay): Coalescer<Partial<TimeOfDay>> => {
        let c = todCoalescers.current.get(field);
        if (!c) {
          c = makeCoalescer<Partial<TimeOfDay>>({
            send: async (patch) => {
              const merged = await client.setTimeOfDay(patch);
              if (!useEditorStore.getState().dragActive) {
                useEditorStore.getState().setEnvironment(merged);
              }
            },
          });
          todCoalescers.current.set(field, c);
        }
        return c;
      },
    [],
  );

  // Undo capture: a gesture touches exactly one field, captured on its first tick and
  // recorded as one entry at drag end; a discrete edit records inline. The shared drag
  // bracket needs no per-field binding because each block patch carries the
  // field. Declared before the early return so the hook count never changes between renders.
  const gesturing = useRef(false);
  const envGesture = useRef<{
    block: "env" | "atmos" | "fog" | "cloud" | "wind" | "tod";
    field: string;
    prior: unknown;
  } | null>(null);

  if (!environment) {
    return (
      <div className="flex h-full min-h-0 flex-col">
        <div className="p-3.5 text-center italic text-muted-foreground">
          {ready ? "Loading environment…" : "Engine not ready"}
        </div>
      </div>
    );
  }

  const env = environment;

  // Record one scene-tab undo entry for an environment / atmosphere field (scene-global,
  // no selection); a no-op is dropped. Replay re-sends the same merge command.
  const recordEnvEdit = (field: keyof Environment, prior: unknown, after: unknown): void => {
    if (JSON.stringify(prior) === JSON.stringify(after)) {
      return;
    }
    useEditorStore.getState().pushEdit(
      {
        label: humanizeFieldName(field),
        undo: () => client.setEnvironment({ [field]: prior } as Partial<Environment>),
        redo: () => client.setEnvironment({ [field]: after } as Partial<Environment>),
      },
      "scene",
    );
  };
  const recordAtmosEdit = (field: keyof Atmosphere, prior: unknown, after: unknown): void => {
    if (JSON.stringify(prior) === JSON.stringify(after)) {
      return;
    }
    useEditorStore.getState().pushEdit(
      {
        label: humanizeFieldName(field),
        undo: () => client.setAtmosphere({ [field]: prior } as Partial<Atmosphere>),
        redo: () => client.setAtmosphere({ [field]: after } as Partial<Atmosphere>),
      },
      "scene",
    );
  };
  const recordFogEdit = (field: keyof Fog, prior: unknown, after: unknown): void => {
    if (JSON.stringify(prior) === JSON.stringify(after)) {
      return;
    }
    useEditorStore.getState().pushEdit(
      {
        label: humanizeFieldName(field),
        undo: () => client.setFog({ [field]: prior } as Partial<Fog>),
        redo: () => client.setFog({ [field]: after } as Partial<Fog>),
      },
      "scene",
    );
  };
  const recordCloudEdit = (field: keyof Cloud, prior: unknown, after: unknown): void => {
    if (JSON.stringify(prior) === JSON.stringify(after)) {
      return;
    }
    useEditorStore.getState().pushEdit(
      {
        label: humanizeFieldName(field),
        undo: () => client.setClouds({ [field]: prior } as Partial<Cloud>),
        redo: () => client.setClouds({ [field]: after } as Partial<Cloud>),
      },
      "scene",
    );
  };
  const recordWindEdit = (field: keyof Wind, prior: unknown, after: unknown): void => {
    if (JSON.stringify(prior) === JSON.stringify(after)) {
      return;
    }
    useEditorStore.getState().pushEdit(
      {
        label: humanizeFieldName(field),
        undo: () => client.setWind({ [field]: prior } as Partial<Wind>),
        redo: () => client.setWind({ [field]: after } as Partial<Wind>),
      },
      "scene",
    );
  };
  const recordTodEdit = (field: keyof TimeOfDay, prior: unknown, after: unknown): void => {
    if (JSON.stringify(prior) === JSON.stringify(after)) {
      return;
    }
    useEditorStore.getState().pushEdit(
      {
        label: humanizeFieldName(field),
        undo: () => client.setTimeOfDay({ [field]: prior } as Partial<TimeOfDay>),
        redo: () => client.setTimeOfDay({ [field]: after } as Partial<TimeOfDay>),
      },
      "scene",
    );
  };

  // Optimistic local write + coalesced send of the one changed field. A discrete edit
  // records immediately; a gesture captures its field + prior on the first tick.
  const patch = (field: keyof Environment, value: Environment[keyof Environment]): void => {
    setActiveProfileKey("custom");
    if (gesturing.current) {
      if (envGesture.current === null) {
        envGesture.current = { block: "env", field, prior: structuredClone(env[field]) };
      }
    } else {
      recordEnvEdit(field, structuredClone(env[field]), structuredClone(value));
    }
    setEnvironment({ ...env, [field]: value } as Environment);
    coalescerFor(field).push({ [field]: value } as Partial<Environment>);
  };

  const onDragStart = (): void => {
    setDragActive(true);
    gesturing.current = true;
    envGesture.current = null;
  };
  const onDragEnd = (): void => {
    setDragActive(false);
    gesturing.current = false;
    const g = envGesture.current;
    envGesture.current = null;
    const live = useEditorStore.getState().environment;
    if (!g || !live) {
      return;
    }
    if (g.block === "atmos") {
      recordAtmosEdit(
        g.field as keyof Atmosphere,
        g.prior,
        structuredClone(live.atmosphere[g.field as keyof Atmosphere]),
      );
    } else if (g.block === "fog") {
      recordFogEdit(g.field as keyof Fog, g.prior, structuredClone(live.fog[g.field as keyof Fog]));
    } else if (g.block === "cloud") {
      recordCloudEdit(
        g.field as keyof Cloud,
        g.prior,
        structuredClone(live.cloud[g.field as keyof Cloud]),
      );
    } else if (g.block === "wind") {
      recordWindEdit(
        g.field as keyof Wind,
        g.prior,
        structuredClone(live.wind[g.field as keyof Wind]),
      );
    } else if (g.block === "tod") {
      recordTodEdit(
        g.field as keyof TimeOfDay,
        g.prior,
        structuredClone(live.timeOfDay[g.field as keyof TimeOfDay]),
      );
    } else {
      recordEnvEdit(
        g.field as keyof Environment,
        g.prior,
        structuredClone(live[g.field as keyof Environment]),
      );
    }
  };

  const onVecChannel =
    (field: "clearColor" | "ambientColor") =>
    (channels: Record<string, number>): void => {
      const next = { ...(env[field] as Vec3), ...channels } as Vec3;
      patch(field, next);
    };

  // Optimistic local write of one atmosphere field + a coalesced `set-atmosphere`
  // merge (a Partial<Atmosphere>). The server folds it over the current block and
  // re-bakes the LUT chain next frame; the merged environment round-trips back.
  const atmos = env.atmosphere;
  const patchAtmos = <K extends keyof Atmosphere>(field: K, value: Atmosphere[K]): void => {
    setActiveProfileKey("custom");
    if (gesturing.current) {
      if (envGesture.current === null) {
        envGesture.current = { block: "atmos", field, prior: structuredClone(atmos[field]) };
      }
    } else {
      recordAtmosEdit(field, structuredClone(atmos[field]), structuredClone(value));
    }
    setEnvironment({ ...env, atmosphere: { ...atmos, [field]: value } } as Environment);
    atmosCoalescerFor(field).push({ [field]: value } as Partial<Atmosphere>);
  };
  const onAtmosVec =
    (field: "rayleighScattering" | "ozoneAbsorption") =>
    (channels: Record<string, number>): void => {
      patchAtmos(field, { ...(atmos[field] as Vec3), ...channels } as Atmosphere[typeof field]);
    };

  // Optimistic local write of one fog field + a coalesced `set-fog` merge (a Partial<Fog>).
  // The server folds it over the current block; the height-fog composite picks it up next frame.
  const fog = env.fog;
  const patchFog = <K extends keyof Fog>(field: K, value: Fog[K]): void => {
    setActiveProfileKey("custom");
    if (gesturing.current) {
      if (envGesture.current === null) {
        envGesture.current = { block: "fog", field, prior: structuredClone(fog[field]) };
      }
    } else {
      recordFogEdit(field, structuredClone(fog[field]), structuredClone(value));
    }
    setEnvironment({ ...env, fog: { ...fog, [field]: value } } as Environment);
    fogCoalescerFor(field).push({ [field]: value } as Partial<Fog>);
  };
  const onFogVec =
    (field: "albedo" | "emissive" | "directionalColor") =>
    (channels: Record<string, number>): void => {
      patchFog(field, { ...(fog[field] as Vec3), ...channels } as Fog[typeof field]);
    };

  const cloud = env.cloud;
  const patchCloud = <K extends keyof Cloud>(field: K, value: Cloud[K]): void => {
    setActiveProfileKey("custom");
    if (gesturing.current) {
      if (envGesture.current === null) {
        envGesture.current = { block: "cloud", field, prior: structuredClone(cloud[field]) };
      }
    } else {
      recordCloudEdit(field, structuredClone(cloud[field]), structuredClone(value));
    }
    setEnvironment({ ...env, cloud: { ...cloud, [field]: value } } as Environment);
    cloudCoalescerFor(field).push({ [field]: value } as Partial<Cloud>);
  };
  const onCloudVec = (channels: Record<string, number>): void => {
    patchCloud("weatherOffset", { ...cloud.weatherOffset, ...channels });
  };

  const wind = env.wind;
  const patchWind = <K extends keyof Wind>(field: K, value: Wind[K]): void => {
    setActiveProfileKey("custom");
    if (gesturing.current) {
      if (envGesture.current === null) {
        envGesture.current = { block: "wind", field, prior: structuredClone(wind[field]) };
      }
    } else {
      recordWindEdit(field, structuredClone(wind[field]), structuredClone(value));
    }
    setEnvironment({ ...env, wind: { ...wind, [field]: value } } as Environment);
    windCoalescerFor(field).push({ [field]: value } as Partial<Wind>);
  };

  const tod = env.timeOfDay;
  const patchTod = <K extends keyof TimeOfDay>(field: K, value: TimeOfDay[K]): void => {
    setActiveProfileKey("custom");
    if (gesturing.current) {
      if (envGesture.current === null) {
        envGesture.current = { block: "tod", field, prior: structuredClone(tod[field]) };
      }
    } else {
      recordTodEdit(field, structuredClone(tod[field]), structuredClone(value));
    }
    setEnvironment({ ...env, timeOfDay: { ...tod, [field]: value } } as Environment);
    todCoalescerFor(field).push({ [field]: value } as Partial<TimeOfDay>);
  };

  const query = search.trim().toLowerCase();
  const showGroup = (section: EnvironmentSection, keywords: string): boolean =>
    query.length === 0 ? viewState.section === section : keywords.toLowerCase().includes(query);
  const sectionOpen = (id: string): boolean => query.length > 0 || viewState.expanded[id] === true;
  const setSectionOpen = (id: string, open: boolean): void =>
    setViewState((current) => ({
      ...current,
      expanded: { ...current.expanded, [id]: open },
    }));
  const isModified = (current: unknown, initial: unknown): boolean =>
    initial !== undefined && JSON.stringify(current) !== JSON.stringify(initial);

  const replaceSnapshot = (label: string, next: Environment): void => {
    const prior = structuredClone(env);
    if (JSON.stringify(prior) === JSON.stringify(next)) return;
    setActiveProfileKey("custom");
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

  const resetTopLevel = (label: string, fields: (keyof Environment)[]): void => {
    if (!defaults) return;
    const next = structuredClone(env);
    for (const field of fields) {
      (next as unknown as Record<keyof Environment, Environment[keyof Environment]>)[field] =
        structuredClone(defaults[field]);
    }
    replaceSnapshot(label, next);
  };

  const resetNested = <B extends "atmosphere" | "cloud" | "fog" | "wind" | "timeOfDay">(
    label: string,
    block: B,
    fields?: (keyof Environment[B])[],
  ): void => {
    if (!defaults) return;
    const next = structuredClone(env);
    if (!fields) {
      next[block] = structuredClone(defaults[block]) as Environment[B];
    } else {
      for (const field of fields) {
        (
          next[block] as unknown as Record<
            keyof Environment[B],
            Environment[B][keyof Environment[B]]
          >
        )[field] = structuredClone(defaults[block][field]);
      }
    }
    replaceSnapshot(label, next);
  };

  const applyProfile = (key: string): void => {
    if (key === "custom") return;
    const profile = profiles.find((candidate) => profileKey(candidate.reference) === key);
    if (!profile) return;
    const prior = structuredClone(env);
    void client
      .applyEnvironmentProfile(profile.reference)
      .then((next) => {
        setEnvironment(next);
        setActiveProfileKey(key);
        useEditorStore.getState().pushEdit(
          {
            label: `Apply ${profile.name}`,
            undo: () => client.replaceEnvironment(prior),
            redo: () => client.applyEnvironmentProfile(profile.reference),
          },
          "scene",
        );
      })
      .catch((error: unknown) => notifyError(errorText(error)));
  };

  const saveProfile = (): void => {
    const name = saveName.trim();
    if (!name) return;
    void client
      .saveEnvironmentProfile(name)
      .then(async (saved) => {
        const list = await client.listEnvironmentProfiles();
        setProfiles(list.profiles);
        setActiveProfileKey(profileKey(saved.reference));
        setSaveOpen(false);
        setSaveName("");
        notify(`Saved environment profile “${saved.name}”`);
      })
      .catch((error: unknown) => notifyError(errorText(error)));
  };

  const activeProfile = profiles.find(
    (profile) => profileKey(profile.reference) === activeProfileKey,
  );
  const activeProjectProfile =
    activeProfile?.reference.kind === "asset"
      ? { id: activeProfile.reference.id, name: activeProfile.name }
      : null;
  const allDetails = viewState.detail === "all" || query.length > 0;

  return (
    <div className="flex h-full min-h-0 flex-col">
      <div className="flex flex-col gap-2 border-b border-border p-2.5">
        <div className="flex gap-1.5">
          <Select value={activeProfileKey} onValueChange={applyProfile}>
            <SelectTrigger size="sm" className="h-7 min-w-0 flex-1 text-[11px]">
              <SelectValue placeholder="Custom environment" />
            </SelectTrigger>
            <SelectContent>
              <SelectItem value="custom" className="text-[11px]">
                Custom environment
              </SelectItem>
              {profiles.map((profile) => (
                <SelectItem
                  key={profileKey(profile.reference)}
                  value={profileKey(profile.reference)}
                  className="text-[11px]"
                >
                  {profile.name}
                </SelectItem>
              ))}
            </SelectContent>
          </Select>
          <Button
            type="button"
            variant="outline"
            size="icon-xs"
            aria-label="Save environment profile"
            title="Save environment profile"
            onClick={() => setSaveOpen(true)}
          >
            <Save />
          </Button>
          {activeProjectProfile ? (
            <Button
              type="button"
              variant="outline"
              size="xs"
              onClick={() => {
                void client
                  .updateEnvironmentProfile(activeProjectProfile.id)
                  .then(() => notify(`Updated environment profile “${activeProjectProfile.name}”`))
                  .catch((error: unknown) => notifyError(errorText(error)));
              }}
            >
              Update
            </Button>
          ) : null}
        </div>

        <div className="relative">
          <Search className="pointer-events-none absolute top-1/2 left-2 size-3.5 -translate-y-1/2 text-muted-foreground" />
          <Input
            value={search}
            onChange={(event) => setSearch(event.target.value)}
            placeholder="Search environment settings"
            className="h-7 pl-7 text-[11px]"
          />
        </div>

        <Tabs
          value={viewState.section}
          onValueChange={(section) =>
            setViewState((current) => ({
              ...current,
              section: section as EnvironmentSection,
            }))
          }
          className="gap-0"
        >
          <TabsList>
            <TabsTrigger value="sky">Sky</TabsTrigger>
            <TabsTrigger value="time">Time</TabsTrigger>
            <TabsTrigger value="weather">Weather</TabsTrigger>
            <TabsTrigger value="fog">Fog</TabsTrigger>
          </TabsList>
        </Tabs>

        <div className="flex items-center justify-between">
          <span className="text-[10px] text-muted-foreground">
            {query ? "Showing matches across all groups" : "Detail level"}
          </span>
          <Select
            value={viewState.detail}
            onValueChange={(detail) =>
              setViewState((current) => ({ ...current, detail: detail as DetailLevel }))
            }
          >
            <SelectTrigger size="sm" className="h-6 w-[92px] text-[10px]">
              <SelectValue />
            </SelectTrigger>
            <SelectContent>
              <SelectItem value="essential" className="text-[11px]">
                Essential
              </SelectItem>
              <SelectItem value="all" className="text-[11px]">
                All
              </SelectItem>
            </SelectContent>
          </Select>
        </div>
      </div>
      <ScrollArea className="min-h-0 flex-1">
        <div className="flex flex-col gap-2 p-2.5">
          {showGroup(
            "sky",
            "sky background mode color texture procedural intensity rotation visible",
          ) ? (
            <PropertySection
              title="Background"
              summary={`${env.skyMode} · ${env.skyIntensity.toFixed(2)}×`}
              open={sectionOpen("background")}
              onOpenChange={(open) => setSectionOpen("background", open)}
              modified={
                defaults !== null &&
                (
                  [
                    "skyMode",
                    "clearColor",
                    "skyTexture",
                    "skyIntensity",
                    "skyRotation",
                    "visible",
                  ] as const
                ).some((field) => isModified(env[field], defaults[field]))
              }
              onReset={() =>
                resetTopLevel("Reset sky background", [
                  "skyMode",
                  "clearColor",
                  "skyTexture",
                  "skyIntensity",
                  "skyRotation",
                  "visible",
                ])
              }
            >
              <Row label="Sky Mode">
                <Select
                  value={env.skyMode}
                  onValueChange={(value) => patch("skyMode", value as SkyMode)}
                >
                  <SelectTrigger size="sm" className="h-7 w-full font-mono text-[11px]">
                    <SelectValue />
                  </SelectTrigger>
                  <SelectContent>
                    {SKY_MODES.map((m) => (
                      <SelectItem key={m.value} value={m.value} className="text-[11px]">
                        {m.label}
                      </SelectItem>
                    ))}
                  </SelectContent>
                </Select>
              </Row>

              {env.skyMode === "color" ? (
                <Row label="Clear Color">
                  <ColorField
                    kind="color3"
                    value={env.clearColor as unknown as Record<string, number>}
                    onChange={onVecChannel("clearColor")}
                    onDragStart={onDragStart}
                    onDragEnd={onDragEnd}
                  />
                </Row>
              ) : null}

              {env.skyMode === "texture" ? (
                <Row label="Sky Texture">
                  <AssetPicker
                    value={env.skyTexture}
                    assetType="texture"
                    onChange={(id) => patch("skyTexture", id)}
                  />
                </Row>
              ) : null}

              <Row label="Intensity">
                <NumberDrag
                  value={env.skyIntensity}
                  min={0}
                  max={100}
                  step={0.01}
                  onChange={(v) => patch("skyIntensity", v)}
                  onDragStart={onDragStart}
                  onDragEnd={onDragEnd}
                />
              </Row>

              {env.skyMode !== "color" ? (
                <Row label="Rotation (°)">
                  <NumberDrag
                    value={env.skyRotation * RAD_TO_DEG}
                    min={-360}
                    max={360}
                    step={0.5}
                    onChange={(deg) => patch("skyRotation", deg * DEG_TO_RAD)}
                    onDragStart={onDragStart}
                    onDragEnd={onDragEnd}
                  />
                </Row>
              ) : null}

              <Row label="Visible">
                <Switch
                  checked={env.visible}
                  onCheckedChange={(checked) => patch("visible", checked)}
                />
              </Row>
            </PropertySection>
          ) : null}

          {showGroup(
            "sky",
            "environment lighting fallback ambient source color intensity directional sky",
          ) ? (
            <PropertySection
              title="Environment lighting"
              summary={env.useSkyForAmbient ? "Authored fallback" : "Directional fallback"}
              open={sectionOpen("lighting")}
              onOpenChange={(open) => setSectionOpen("lighting", open)}
              modified={
                defaults !== null &&
                (["useSkyForAmbient", "ambientColor", "ambientIntensity"] as const).some((field) =>
                  isModified(env[field], defaults[field]),
                )
              }
              onReset={() =>
                resetTopLevel("Reset environment lighting", [
                  "useSkyForAmbient",
                  "ambientColor",
                  "ambientIntensity",
                ])
              }
            >
              <Row label="Fallback source">
                <Select
                  value={env.useSkyForAmbient ? "authored" : "directional"}
                  onValueChange={(value) => patch("useSkyForAmbient", value === "authored")}
                >
                  <SelectTrigger size="sm" className="h-7 w-full text-[11px]">
                    <SelectValue />
                  </SelectTrigger>
                  <SelectContent>
                    <SelectItem value="authored" className="text-[11px]">
                      Authored color
                    </SelectItem>
                    <SelectItem value="directional" className="text-[11px]">
                      Directional light
                    </SelectItem>
                  </SelectContent>
                </Select>
              </Row>

              {env.useSkyForAmbient ? (
                <>
                  <Row label="Ambient Color">
                    <ColorField
                      kind="color3"
                      value={env.ambientColor as unknown as Record<string, number>}
                      onChange={onVecChannel("ambientColor")}
                      onDragStart={onDragStart}
                      onDragEnd={onDragEnd}
                    />
                  </Row>

                  <Row label="Ambient Int.">
                    <NumberDrag
                      value={env.ambientIntensity}
                      min={0}
                      max={10}
                      step={0.005}
                      onChange={(v) => patch("ambientIntensity", v)}
                      onDragStart={onDragStart}
                      onDragEnd={onDragEnd}
                    />
                  </Row>
                </>
              ) : null}
            </PropertySection>
          ) : null}

          {showGroup(
            "time",
            "time of day clock playback sun manual date year month day latitude longitude location automation curves exposure tint coverage cloud type",
          ) ? (
            <PropertySection
              title="Clock and location"
              summary={tod.enabled ? `${(tod.timeOfDay * 24).toFixed(1)} h` : "Disabled"}
              open={sectionOpen("clock")}
              onOpenChange={(open) => setSectionOpen("clock", open)}
              modified={defaults !== null && isModified(tod, defaults.timeOfDay)}
              onReset={() => resetNested("Reset time of day", "timeOfDay")}
            >
              <Row label="Time of Day">
                <Switch
                  checked={tod.enabled}
                  onCheckedChange={(checked) => patchTod("enabled", checked)}
                />
              </Row>

              {tod.enabled ? (
                <>
                  <Row label="Manual Sun">
                    <Switch
                      checked={tod.manualOverride}
                      onCheckedChange={(checked) => patchTod("manualOverride", checked)}
                    />
                  </Row>

                  <Row label="Time (hours)">
                    <NumberDrag
                      value={tod.timeOfDay * 24}
                      min={0}
                      max={24}
                      step={0.01}
                      onChange={(value) => patchTod("timeOfDay", value / 24)}
                      onDragStart={onDragStart}
                      onDragEnd={onDragEnd}
                    />
                  </Row>

                  <Row label="Year">
                    <NumberDrag
                      value={tod.year}
                      min={-2000}
                      max={6000}
                      step={1}
                      onChange={(value) => patchTod("year", Math.round(value))}
                      onDragStart={onDragStart}
                      onDragEnd={onDragEnd}
                    />
                  </Row>

                  <Row label="Month">
                    <NumberDrag
                      value={tod.month}
                      min={1}
                      max={12}
                      step={1}
                      onChange={(value) => patchTod("month", Math.round(value))}
                      onDragStart={onDragStart}
                      onDragEnd={onDragEnd}
                    />
                  </Row>

                  <Row label="Day">
                    <NumberDrag
                      value={tod.day}
                      min={1}
                      max={31}
                      step={1}
                      onChange={(value) => patchTod("day", Math.round(value))}
                      onDragStart={onDragStart}
                      onDragEnd={onDragEnd}
                    />
                  </Row>

                  <Row label="Latitude">
                    <NumberDrag
                      value={tod.latitude}
                      min={-90}
                      max={90}
                      step={0.01}
                      onChange={(value) => patchTod("latitude", value)}
                      onDragStart={onDragStart}
                      onDragEnd={onDragEnd}
                    />
                  </Row>

                  <Row label="Longitude">
                    <NumberDrag
                      value={tod.longitude}
                      min={-180}
                      max={180}
                      step={0.01}
                      onChange={(value) => patchTod("longitude", value)}
                      onDragStart={onDragStart}
                      onDragEnd={onDragEnd}
                    />
                  </Row>

                  <Row label="Day Seconds">
                    <NumberDrag
                      value={tod.dayLengthSeconds}
                      min={0}
                      max={86400}
                      step={1}
                      onChange={(value) => patchTod("dayLengthSeconds", value)}
                      onDragStart={onDragStart}
                      onDragEnd={onDragEnd}
                    />
                  </Row>

                  <Row label="Automation">
                    <Button
                      type="button"
                      variant="outline"
                      size="xs"
                      className="w-full"
                      onClick={() => setCurvesOpen(true)}
                    >
                      Edit appearance curves
                    </Button>
                  </Row>
                </>
              ) : null}
            </PropertySection>
          ) : null}

          {showGroup(
            "sky",
            "physical atmosphere rayleigh mie ozone planet radius height sun moon celestial earthshine",
          ) ? (
            <PropertySection
              title="Physical atmosphere"
              summary={atmos.enabled ? "Enabled" : "Disabled"}
              open={sectionOpen("atmosphere")}
              onOpenChange={(open) => setSectionOpen("atmosphere", open)}
              modified={
                defaults !== null &&
                (
                  [
                    "enabled",
                    "planetRadius",
                    "atmosphereHeight",
                    "rayleighScattering",
                    "rayleighScaleHeight",
                    "mieScattering",
                    "mieScaleHeight",
                    "mieAnisotropy",
                    "ozoneAbsorption",
                    "sunDiskAngularRadius",
                    "sunDiskIntensity",
                    "moonDiskAngularRadius",
                    "moonDiskIntensity",
                    "moonEarthshine",
                  ] as const
                ).some((field) => isModified(atmos[field], defaults.atmosphere[field]))
              }
              onReset={() =>
                resetNested("Reset physical atmosphere", "atmosphere", [
                  "enabled",
                  "planetRadius",
                  "atmosphereHeight",
                  "rayleighScattering",
                  "rayleighScaleHeight",
                  "mieScattering",
                  "mieScaleHeight",
                  "mieAnisotropy",
                  "ozoneAbsorption",
                  "sunDiskAngularRadius",
                  "sunDiskIntensity",
                  "moonDiskAngularRadius",
                  "moonDiskIntensity",
                  "moonEarthshine",
                ])
              }
            >
              <Row label="Atmosphere">
                <Switch
                  checked={atmos.enabled}
                  onCheckedChange={(checked) => patchAtmos("enabled", checked)}
                />
              </Row>

              {atmos.enabled ? (
                <>
                  {allDetails ? (
                    <>
                      <Row label="Planet radius">
                        <NumberDrag
                          value={atmos.planetRadius}
                          min={100}
                          max={100000}
                          step={1}
                          onChange={(value) => patchAtmos("planetRadius", value)}
                          onDragStart={onDragStart}
                          onDragEnd={onDragEnd}
                        />
                      </Row>
                      <Row label="Atmos. height">
                        <NumberDrag
                          value={atmos.atmosphereHeight}
                          min={1}
                          max={1000}
                          step={1}
                          onChange={(value) => patchAtmos("atmosphereHeight", value)}
                          onDragStart={onDragStart}
                          onDragEnd={onDragEnd}
                        />
                      </Row>
                      <Row label="Rayleigh">
                        <VectorEditor
                          axes={["x", "y", "z"]}
                          labels={["R", "G", "B"]}
                          value={atmos.rayleighScattering as unknown as Record<string, number>}
                          step={0.1}
                          onChange={onAtmosVec("rayleighScattering")}
                          onDragStart={onDragStart}
                          onDragEnd={onDragEnd}
                        />
                      </Row>

                      <Row label="Rayleigh Ht.">
                        <NumberDrag
                          value={atmos.rayleighScaleHeight}
                          min={0.1}
                          max={60}
                          step={0.1}
                          onChange={(v) => patchAtmos("rayleighScaleHeight", v)}
                          onDragStart={onDragStart}
                          onDragEnd={onDragEnd}
                        />
                      </Row>
                    </>
                  ) : null}

                  <Row label="Mie">
                    <NumberDrag
                      value={atmos.mieScattering}
                      min={0}
                      max={50}
                      step={0.01}
                      onChange={(v) => patchAtmos("mieScattering", v)}
                      onDragStart={onDragStart}
                      onDragEnd={onDragEnd}
                    />
                  </Row>

                  {allDetails ? (
                    <Row label="Mie Ht.">
                      <NumberDrag
                        value={atmos.mieScaleHeight}
                        min={0.1}
                        max={20}
                        step={0.05}
                        onChange={(v) => patchAtmos("mieScaleHeight", v)}
                        onDragStart={onDragStart}
                        onDragEnd={onDragEnd}
                      />
                    </Row>
                  ) : null}

                  <Row label="Mie Aniso.">
                    <NumberDrag
                      value={atmos.mieAnisotropy}
                      min={-0.99}
                      max={0.99}
                      step={0.005}
                      onChange={(v) => patchAtmos("mieAnisotropy", v)}
                      onDragStart={onDragStart}
                      onDragEnd={onDragEnd}
                    />
                  </Row>

                  {allDetails ? (
                    <Row label="Ozone">
                      <VectorEditor
                        axes={["x", "y", "z"]}
                        labels={["R", "G", "B"]}
                        value={atmos.ozoneAbsorption as unknown as Record<string, number>}
                        step={0.01}
                        onChange={onAtmosVec("ozoneAbsorption")}
                        onDragStart={onDragStart}
                        onDragEnd={onDragEnd}
                      />
                    </Row>
                  ) : null}

                  {allDetails ? (
                    <Row label="Sun radius">
                      <NumberDrag
                        value={atmos.sunDiskAngularRadius}
                        min={0.0001}
                        max={0.05}
                        step={0.00001}
                        onChange={(value) => patchAtmos("sunDiskAngularRadius", value)}
                        onDragStart={onDragStart}
                        onDragEnd={onDragEnd}
                      />
                    </Row>
                  ) : null}

                  <Row label="Sun Disk">
                    <NumberDrag
                      value={atmos.sunDiskIntensity}
                      min={0}
                      max={100}
                      step={0.1}
                      onChange={(v) => patchAtmos("sunDiskIntensity", v)}
                      onDragStart={onDragStart}
                      onDragEnd={onDragEnd}
                    />
                  </Row>

                  {allDetails ? (
                    <Row label="Moon Radius">
                      <NumberDrag
                        value={atmos.moonDiskAngularRadius}
                        min={0.0001}
                        max={0.05}
                        step={0.00001}
                        onChange={(v) => patchAtmos("moonDiskAngularRadius", v)}
                        onDragStart={onDragStart}
                        onDragEnd={onDragEnd}
                      />
                    </Row>
                  ) : null}

                  <Row label="Moon Disk">
                    <NumberDrag
                      value={atmos.moonDiskIntensity}
                      min={0}
                      max={100}
                      step={0.01}
                      onChange={(v) => patchAtmos("moonDiskIntensity", v)}
                      onDragStart={onDragStart}
                      onDragEnd={onDragEnd}
                    />
                  </Row>

                  {allDetails ? (
                    <Row label="Earthshine">
                      <NumberDrag
                        value={atmos.moonEarthshine}
                        min={0}
                        max={1}
                        step={0.001}
                        onChange={(v) => patchAtmos("moonEarthshine", v)}
                        onDragStart={onDragStart}
                        onDragEnd={onDragEnd}
                      />
                    </Row>
                  ) : null}
                </>
              ) : null}
            </PropertySection>
          ) : null}

          {showGroup(
            "weather",
            "clouds coverage type precipitation anvil layer altitude height shape noise detail curl weather map droplet shadows",
          ) ? (
            <PropertySection
              title="Clouds"
              summary={cloud.enabled ? `${Math.round(cloud.coverage * 100)}% coverage` : "Disabled"}
              open={sectionOpen("clouds")}
              onOpenChange={(open) => setSectionOpen("clouds", open)}
              modified={
                defaults !== null &&
                (Object.keys(cloud) as (keyof Cloud)[])
                  .filter(
                    (field) => !["primarySteps", "lightSteps", "temporalFactor"].includes(field),
                  )
                  .some((field) => isModified(cloud[field], defaults.cloud[field]))
              }
              onReset={() =>
                resetNested(
                  "Reset clouds",
                  "cloud",
                  (Object.keys(cloud) as (keyof Cloud)[]).filter(
                    (field) => !["primarySteps", "lightSteps", "temporalFactor"].includes(field),
                  ),
                )
              }
            >
              <Row label="Clouds">
                <Switch
                  checked={cloud.enabled}
                  onCheckedChange={(checked) => patchCloud("enabled", checked)}
                />
              </Row>

              {cloud.enabled ? (
                <>
                  <Row label="Coverage">
                    <NumberDrag
                      value={cloud.coverage}
                      min={0}
                      max={1}
                      step={0.005}
                      onChange={(value) => patchCloud("coverage", value)}
                      onDragStart={onDragStart}
                      onDragEnd={onDragEnd}
                    />
                  </Row>
                  <Row label="Cloud Type">
                    <NumberDrag
                      value={cloud.cloudType}
                      min={0}
                      max={1}
                      step={0.005}
                      onChange={(value) => patchCloud("cloudType", value)}
                      onDragStart={onDragStart}
                      onDragEnd={onDragEnd}
                    />
                  </Row>
                  <Row label="Precipitation">
                    <NumberDrag
                      value={cloud.precipitation}
                      min={0}
                      max={1}
                      step={0.005}
                      onChange={(value) => patchCloud("precipitation", value)}
                      onDragStart={onDragStart}
                      onDragEnd={onDragEnd}
                    />
                  </Row>
                  {allDetails ? (
                    <>
                      <Row label="Anvil Bias">
                        <NumberDrag
                          value={cloud.anvilBias}
                          min={0}
                          max={1}
                          step={0.005}
                          onChange={(value) => patchCloud("anvilBias", value)}
                          onDragStart={onDragStart}
                          onDragEnd={onDragEnd}
                        />
                      </Row>
                      <Row label="Layer Altitude">
                        <NumberDrag
                          value={cloud.layerAltitude}
                          min={-1000}
                          max={20000}
                          step={10}
                          onChange={(value) => patchCloud("layerAltitude", value)}
                          onDragStart={onDragStart}
                          onDragEnd={onDragEnd}
                        />
                      </Row>
                      <Row label="Layer Height">
                        <NumberDrag
                          value={cloud.layerHeight}
                          min={1}
                          max={20000}
                          step={10}
                          onChange={(value) => patchCloud("layerHeight", value)}
                          onDragStart={onDragStart}
                          onDragEnd={onDragEnd}
                        />
                      </Row>
                      <Row label="Base Scale">
                        <NumberDrag
                          value={cloud.baseScale}
                          min={0.000001}
                          max={0.01}
                          step={0.000001}
                          onChange={(value) => patchCloud("baseScale", value)}
                          onDragStart={onDragStart}
                          onDragEnd={onDragEnd}
                        />
                      </Row>
                      <Row label="Detail Scale">
                        <NumberDrag
                          value={cloud.detailScale}
                          min={0.000001}
                          max={0.1}
                          step={0.00001}
                          onChange={(value) => patchCloud("detailScale", value)}
                          onDragStart={onDragStart}
                          onDragEnd={onDragEnd}
                        />
                      </Row>
                      <Row label="Detail Strength">
                        <NumberDrag
                          value={cloud.detailStrength}
                          min={0}
                          max={1}
                          step={0.005}
                          onChange={(value) => patchCloud("detailStrength", value)}
                          onDragStart={onDragStart}
                          onDragEnd={onDragEnd}
                        />
                      </Row>
                      <Row label="Curl Strength">
                        <NumberDrag
                          value={cloud.curlStrength}
                          min={0}
                          max={2000}
                          step={1}
                          onChange={(value) => patchCloud("curlStrength", value)}
                          onDragStart={onDragStart}
                          onDragEnd={onDragEnd}
                        />
                      </Row>
                      <Row label="Weather Scale">
                        <NumberDrag
                          value={cloud.weatherScale}
                          min={0.000001}
                          max={0.01}
                          step={0.000001}
                          onChange={(value) => patchCloud("weatherScale", value)}
                          onDragStart={onDragStart}
                          onDragEnd={onDragEnd}
                        />
                      </Row>
                      <Row label="Weather Offset">
                        <VectorEditor
                          axes={["x", "z"]}
                          labels={["X", "Z"]}
                          value={cloud.weatherOffset as unknown as Record<string, number>}
                          step={10}
                          onChange={onCloudVec}
                          onDragStart={onDragStart}
                          onDragEnd={onDragEnd}
                        />
                      </Row>
                      <Row label="Weather Map">
                        <AssetPicker
                          value={cloud.weatherTexture}
                          assetType="texture"
                          onChange={(id) => patchCloud("weatherTexture", id)}
                        />
                      </Row>
                      <Row label="Droplet Diameter">
                        <NumberDrag
                          value={cloud.dropletDiameter}
                          min={5}
                          max={50}
                          step={0.1}
                          onChange={(value) => patchCloud("dropletDiameter", value)}
                          onDragStart={onDragStart}
                          onDragEnd={onDragEnd}
                        />
                      </Row>
                    </>
                  ) : null}
                  <Row label="Cast Shadows">
                    <Switch
                      checked={cloud.castCloudShadows}
                      onCheckedChange={(checked) => patchCloud("castCloudShadows", checked)}
                    />
                  </Row>
                  {cloud.castCloudShadows && allDetails ? (
                    <>
                      <Row label="Cloud Shadow">
                        <NumberDrag
                          value={cloud.cloudShadowStrength}
                          min={0}
                          max={1}
                          step={0.005}
                          onChange={(value) => patchCloud("cloudShadowStrength", value)}
                          onDragStart={onDragStart}
                          onDragEnd={onDragEnd}
                        />
                      </Row>
                      <Row label="Surface Shadow">
                        <NumberDrag
                          value={cloud.cloudShadowOnSurfaceStrength}
                          min={0}
                          max={1}
                          step={0.005}
                          onChange={(value) => patchCloud("cloudShadowOnSurfaceStrength", value)}
                          onDragStart={onDragStart}
                          onDragEnd={onDragEnd}
                        />
                      </Row>
                    </>
                  ) : null}
                </>
              ) : null}
            </PropertySection>
          ) : null}

          {showGroup("weather", "wind direction orientation speed gust advection") ? (
            <PropertySection
              title="Wind"
              summary={`${wind.speed.toFixed(1)} m/s`}
              open={sectionOpen("wind")}
              onOpenChange={(open) => setSectionOpen("wind", open)}
              modified={defaults !== null && isModified(wind, defaults.wind)}
              onReset={() => resetNested("Reset wind", "wind")}
            >
              <Row label="Wind Dir.">
                <NumberDrag
                  value={wind.orientation}
                  min={-360}
                  max={360}
                  step={1}
                  onChange={(value) => patchWind("orientation", value)}
                  onDragStart={onDragStart}
                  onDragEnd={onDragEnd}
                />
              </Row>
              <Row label="Wind Speed">
                <NumberDrag
                  value={wind.speed}
                  min={0}
                  max={200}
                  step={0.1}
                  onChange={(value) => patchWind("speed", value)}
                  onDragStart={onDragStart}
                  onDragEnd={onDragEnd}
                />
              </Row>
              <Row label="Wind Gust">
                <NumberDrag
                  value={wind.gust}
                  min={0}
                  max={4}
                  step={0.01}
                  onChange={(value) => patchWind("gust", value)}
                  onDragStart={onDragStart}
                  onDragEnd={onDragEnd}
                />
              </Row>
              <Row label="Turb. Octaves">
                <NumberDrag
                  value={wind.turbulenceOctaves}
                  min={0}
                  max={8}
                  step={1}
                  onChange={(value) => patchWind("turbulenceOctaves", Math.round(value))}
                  onDragStart={onDragStart}
                  onDragEnd={onDragEnd}
                />
              </Row>
              <Row label="Turb. Roughness">
                <NumberDrag
                  value={wind.turbulenceRoughness}
                  min={0}
                  max={1}
                  step={0.01}
                  onChange={(value) => patchWind("turbulenceRoughness", value)}
                  onDragStart={onDragStart}
                  onDragEnd={onDragEnd}
                />
              </Row>
              <Row label="Gust Freq. (Hz)">
                <NumberDrag
                  value={wind.gustFrequency}
                  min={0}
                  max={2}
                  step={0.01}
                  onChange={(value) => patchWind("gustFrequency", value)}
                  onDragStart={onDragStart}
                  onDragEnd={onDragEnd}
                />
              </Row>
              <Row label="Ref. Height (m)">
                <NumberDrag
                  value={wind.referenceHeight}
                  min={0.1}
                  max={200}
                  step={0.1}
                  onChange={(value) => patchWind("referenceHeight", value)}
                  onDragStart={onDragStart}
                  onDragEnd={onDragEnd}
                />
              </Row>
              <Row label="Height Exp.">
                <NumberDrag
                  value={wind.heightExponent}
                  min={0}
                  max={1}
                  step={0.01}
                  onChange={(value) => patchWind("heightExponent", value)}
                  onDragStart={onDragStart}
                  onDragEnd={onDragEnd}
                />
              </Row>
            </PropertySection>
          ) : null}

          {showGroup(
            "fog",
            "fog mode density albedo height falloff distance opacity volumetric medium scatter phase lighting emissive sun ground haze aerial perspective",
          ) ? (
            <PropertySection
              title="Fog appearance"
              summary={fog.enabled ? fog.mode : "Disabled"}
              open={sectionOpen("fogAppearance")}
              onOpenChange={(open) => setSectionOpen("fogAppearance", open)}
              modified={
                defaults !== null &&
                (Object.keys(fog) as (keyof Fog)[])
                  .filter(
                    (field) =>
                      !["quality", "historyBlend", "neighborhoodClamp", "lightClamp"].includes(
                        field,
                      ),
                  )
                  .some((field) => isModified(fog[field], defaults.fog[field]))
              }
              onReset={() =>
                resetNested(
                  "Reset fog",
                  "fog",
                  (Object.keys(fog) as (keyof Fog)[]).filter(
                    (field) =>
                      !["quality", "historyBlend", "neighborhoodClamp", "lightClamp"].includes(
                        field,
                      ),
                  ),
                )
              }
            >
              <Row label="Fog">
                <Switch
                  checked={fog.enabled}
                  onCheckedChange={(checked) => patchFog("enabled", checked)}
                />
              </Row>

              {fog.enabled ? (
                <>
                  <Row label="Mode">
                    <Select
                      value={fog.mode}
                      onValueChange={(value) => patchFog("mode", value as Fog["mode"])}
                    >
                      <SelectTrigger size="sm" className="h-7 w-full font-mono text-[11px]">
                        <SelectValue />
                      </SelectTrigger>
                      <SelectContent>
                        {FOG_MODES.map((m) => (
                          <SelectItem key={m.value} value={m.value} className="text-[11px]">
                            {m.label}
                          </SelectItem>
                        ))}
                      </SelectContent>
                    </Select>
                  </Row>

                  {fog.mode === "volumetric" ? (
                    <>
                      <Row label="Base Density">
                        <NumberDrag
                          value={fog.baseDensity}
                          min={0}
                          max={2}
                          step={0.001}
                          onChange={(v) => patchFog("baseDensity", v)}
                          onDragStart={onDragStart}
                          onDragEnd={onDragEnd}
                        />
                      </Row>

                      <Row label="Scatter Albedo">
                        <NumberDrag
                          value={fog.scatterAlbedo}
                          min={0}
                          max={1}
                          step={0.01}
                          onChange={(v) => patchFog("scatterAlbedo", v)}
                          onDragStart={onDragStart}
                          onDragEnd={onDragEnd}
                        />
                      </Row>

                      <Row label="Phase g">
                        <NumberDrag
                          value={fog.phaseG}
                          min={-0.99}
                          max={0.99}
                          step={0.01}
                          onChange={(v) => patchFog("phaseG", v)}
                          onDragStart={onDragStart}
                          onDragEnd={onDragEnd}
                        />
                      </Row>
                    </>
                  ) : null}

                  <Row label="Density">
                    <NumberDrag
                      value={fog.density}
                      min={0}
                      max={2}
                      step={0.001}
                      onChange={(v) => patchFog("density", v)}
                      onDragStart={onDragStart}
                      onDragEnd={onDragEnd}
                    />
                  </Row>

                  <Row label="Albedo">
                    <ColorField
                      kind="color3"
                      value={fog.albedo as unknown as Record<string, number>}
                      onChange={onFogVec("albedo")}
                      onDragStart={onDragStart}
                      onDragEnd={onDragEnd}
                    />
                  </Row>

                  {allDetails ? (
                    <Row label="Height">
                      <NumberDrag
                        value={fog.height}
                        min={-1000}
                        max={1000}
                        step={0.1}
                        onChange={(v) => patchFog("height", v)}
                        onDragStart={onDragStart}
                        onDragEnd={onDragEnd}
                      />
                    </Row>
                  ) : null}

                  {allDetails ? (
                    <Row label="Height Falloff">
                      <NumberDrag
                        value={fog.heightFalloff}
                        min={0}
                        max={5}
                        step={0.005}
                        onChange={(v) => patchFog("heightFalloff", v)}
                        onDragStart={onDragStart}
                        onDragEnd={onDragEnd}
                      />
                    </Row>
                  ) : null}

                  <Row label="Start Dist.">
                    <NumberDrag
                      value={fog.startDistance}
                      min={0}
                      max={1000}
                      step={0.1}
                      onChange={(v) => patchFog("startDistance", v)}
                      onDragStart={onDragStart}
                      onDragEnd={onDragEnd}
                    />
                  </Row>

                  <Row label="Max Opacity">
                    <NumberDrag
                      value={fog.maxOpacity}
                      min={0}
                      max={1}
                      step={0.005}
                      onChange={(v) => patchFog("maxOpacity", v)}
                      onDragStart={onDragStart}
                      onDragEnd={onDragEnd}
                    />
                  </Row>

                  {allDetails ? (
                    <>
                      <Row label="Emissive">
                        <ColorField
                          kind="color3"
                          value={fog.emissive as unknown as Record<string, number>}
                          onChange={onFogVec("emissive")}
                          onDragStart={onDragStart}
                          onDragEnd={onDragEnd}
                        />
                      </Row>

                      <Row label="Sun Color">
                        <ColorField
                          kind="color3"
                          value={fog.directionalColor as unknown as Record<string, number>}
                          onChange={onFogVec("directionalColor")}
                          onDragStart={onDragStart}
                          onDragEnd={onDragEnd}
                        />
                      </Row>

                      <Row label="Sun Exp.">
                        <NumberDrag
                          value={fog.directionalExponent}
                          min={1}
                          max={64}
                          step={0.1}
                          onChange={(v) => patchFog("directionalExponent", v)}
                          onDragStart={onDragStart}
                          onDragEnd={onDragEnd}
                        />
                      </Row>

                      <Row label="Ground Density">
                        <NumberDrag
                          value={fog.layer2Density}
                          min={0}
                          max={2}
                          step={0.001}
                          onChange={(v) => patchFog("layer2Density", v)}
                          onDragStart={onDragStart}
                          onDragEnd={onDragEnd}
                        />
                      </Row>

                      <Row label="Ground Falloff">
                        <NumberDrag
                          value={fog.layer2Falloff}
                          min={0}
                          max={5}
                          step={0.005}
                          onChange={(v) => patchFog("layer2Falloff", v)}
                          onDragStart={onDragStart}
                          onDragEnd={onDragEnd}
                        />
                      </Row>

                      <Row label="Ground Height">
                        <NumberDrag
                          value={fog.layer2Height}
                          min={-1000}
                          max={1000}
                          step={0.1}
                          onChange={(v) => patchFog("layer2Height", v)}
                          onDragStart={onDragStart}
                          onDragEnd={onDragEnd}
                        />
                      </Row>

                      <Row label="Aerial Persp.">
                        <Switch
                          checked={fog.aerialPerspective}
                          onCheckedChange={(checked) => patchFog("aerialPerspective", checked)}
                        />
                      </Row>

                      {fog.aerialPerspective ? (
                        <Row label="AP Intensity">
                          <NumberDrag
                            value={fog.aerialIntensity}
                            min={0}
                            max={8}
                            step={0.05}
                            onChange={(v) => patchFog("aerialIntensity", v)}
                            onDragStart={onDragStart}
                            onDragEnd={onDragEnd}
                          />
                        </Row>
                      ) : null}
                    </>
                  ) : null}
                </>
              ) : null}
            </PropertySection>
          ) : null}
        </div>
      </ScrollArea>

      <Dialog open={saveOpen} onOpenChange={setSaveOpen} perfLabel="save-environment-profile">
        <DialogContent>
          <DialogHeader>
            <DialogTitle>Save environment profile</DialogTitle>
            <DialogDescription>
              Store the complete current environment as a reusable project asset.
            </DialogDescription>
          </DialogHeader>
          <Input
            value={saveName}
            onChange={(event) => setSaveName(event.target.value)}
            placeholder="Profile name"
            onKeyDown={(event) => {
              if (event.key === "Enter") saveProfile();
            }}
          />
          <DialogFooter>
            <Button type="button" variant="outline" onClick={() => setSaveOpen(false)}>
              Cancel
            </Button>
            <Button type="button" disabled={!saveName.trim()} onClick={saveProfile}>
              Save profile
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>

      <Dialog open={curvesOpen} onOpenChange={setCurvesOpen} perfLabel="environment-curves">
        <DialogContent className="flex h-[min(760px,calc(100%-2rem))] max-w-[min(960px,calc(100%-2rem))] grid-rows-none flex-col sm:max-w-[min(960px,calc(100%-2rem))]">
          <DialogHeader>
            <DialogTitle>Time-of-day appearance curves</DialogTitle>
            <DialogDescription>
              Curves are evaluated from normalized sun elevation and automate the rendered world.
            </DialogDescription>
          </DialogHeader>
          <ScrollArea className="min-h-0 flex-1">
            <div className="grid gap-3 pr-3 lg:grid-cols-2">
              <div className="flex flex-col gap-2 rounded-md border border-border p-3">
                <div className="flex items-center justify-between">
                  <Label>Exposure</Label>
                  <Switch
                    checked={tod.exposureCurve.length > 0}
                    onCheckedChange={(checked) =>
                      patchTod("exposureCurve", checked ? identityCurve() : [])
                    }
                  />
                </div>
                {tod.exposureCurve.length > 0 ? (
                  <ToneCurve
                    channels={emptyScalarChannels(tod.exposureCurve)}
                    visibleChannels={["master"]}
                    masterLabel="EV"
                    onChange={(channels) => patchTod("exposureCurve", channels.master)}
                    onDragStart={onDragStart}
                    onDragEnd={onDragEnd}
                  />
                ) : null}
              </div>

              <div className="flex flex-col gap-2 rounded-md border border-border p-3">
                <div className="flex items-center justify-between">
                  <Label>Sky tint</Label>
                  <Switch
                    checked={Object.values(tod.tintCurve).some((curve) => curve.length > 0)}
                    onCheckedChange={(checked) =>
                      patchTod(
                        "tintCurve",
                        checked
                          ? {
                              master: identityCurve(),
                              red: identityCurve(),
                              green: identityCurve(),
                              blue: identityCurve(),
                            }
                          : { master: [], red: [], green: [], blue: [] },
                      )
                    }
                  />
                </div>
                {Object.values(tod.tintCurve).some((curve) => curve.length > 0) ? (
                  <ToneCurve
                    channels={tintChannels(tod.tintCurve)}
                    onChange={(channels) => patchTod("tintCurve", timeOfDayTint(channels))}
                    onDragStart={onDragStart}
                    onDragEnd={onDragEnd}
                  />
                ) : null}
              </div>

              <div className="flex flex-col gap-2 rounded-md border border-border p-3">
                <div className="flex items-center justify-between">
                  <Label>Cloud coverage</Label>
                  <Switch
                    checked={tod.coverageCurve.length > 0}
                    onCheckedChange={(checked) =>
                      patchTod("coverageCurve", checked ? identityCurve() : [])
                    }
                  />
                </div>
                {tod.coverageCurve.length > 0 ? (
                  <ToneCurve
                    channels={emptyScalarChannels(tod.coverageCurve)}
                    visibleChannels={["master"]}
                    masterLabel="Cloud"
                    onChange={(channels) => patchTod("coverageCurve", channels.master)}
                    onDragStart={onDragStart}
                    onDragEnd={onDragEnd}
                  />
                ) : null}
              </div>

              <div className="flex flex-col gap-2 rounded-md border border-border p-3">
                <div className="flex items-center justify-between">
                  <Label>Cloud type</Label>
                  <Switch
                    checked={tod.cloudTypeCurve.length > 0}
                    onCheckedChange={(checked) =>
                      patchTod("cloudTypeCurve", checked ? identityCurve() : [])
                    }
                  />
                </div>
                {tod.cloudTypeCurve.length > 0 ? (
                  <ToneCurve
                    channels={emptyScalarChannels(tod.cloudTypeCurve)}
                    visibleChannels={["master"]}
                    masterLabel="Type"
                    onChange={(channels) => patchTod("cloudTypeCurve", channels.master)}
                    onDragStart={onDragStart}
                    onDragEnd={onDragEnd}
                  />
                ) : null}
              </div>
            </div>
          </ScrollArea>
          <DialogFooter>
            <Button type="button" onClick={() => setCurvesOpen(false)}>
              Done
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </div>
  );
}
