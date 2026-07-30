import { useEffect, useState } from "react";
import { Save, Search } from "lucide-react";
import { client } from "../../control/client";
import { useEditorStore } from "../../state/store";
import { errorText, notify, notifyError } from "../../lib/flash";
import type { Environment, EnvironmentProfileSummaryDto } from "../../protocol";
import { Button } from "@/components/ui/button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { Input } from "@/components/ui/input";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { Tabs, TabsList, TabsTrigger } from "@/components/ui/tabs";
import { EnvironmentBody } from "./EnvironmentBody";
import {
  loadViewState,
  saveViewState,
  type DetailLevel,
  type EnvironmentSection,
} from "./viewState";

function profileKey(profile: EnvironmentProfileSummaryDto["reference"]): string {
  return profile.kind === "asset" ? `asset:${profile.id}` : `builtin:${profile.profile}`;
}

/// The Environment panel owns the rendered world: sky, time, weather, and fog. Renderer-cost
/// controls live in Render and image-formation controls in Post; `SceneEnvironment.exposure` is
/// reserved on the wire, so the effective tonemap exposure is the render-side `set-exposure`.
export function EnvironmentPanel() {
  const phase = useEditorStore((s) => s.engineStatus.phase);
  const sceneVersion = useEditorStore((s) => s.sceneVersion);
  const environment = useEditorStore((s) => s.environment);

  const ready = phase === "ready";
  const [viewState, setViewState] = useState(loadViewState);
  const [search, setSearch] = useState("");
  const [defaults, setDefaults] = useState<Environment | null>(null);
  const [profiles, setProfiles] = useState<EnvironmentProfileSummaryDto[]>([]);
  const [activeProfileKey, setActiveProfileKey] = useState("custom");
  const [saveOpen, setSaveOpen] = useState(false);
  const [saveName, setSaveName] = useState("");

  useEffect(() => {
    saveViewState(viewState);
  }, [viewState]);

  // The reconcile poll also refreshes on a scene change; this fetch guarantees the panel is correct
  // even if the poll's focus/drag gate skipped that tick.
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
    if (!ready) {
      return;
    }
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

  if (!environment) {
    return (
      <div className="flex h-full min-h-0 flex-col">
        <div className="p-3.5 text-center italic text-muted-foreground">
          {ready ? "Loading environment…" : "Engine not ready"}
        </div>
      </div>
    );
  }

  const applyProfile = (key: string): void => {
    if (key === "custom") {
      return;
    }
    const profile = profiles.find((candidate) => profileKey(candidate.reference) === key);
    if (!profile) {
      return;
    }
    const prior = structuredClone(environment);
    void client
      .applyEnvironmentProfile(profile.reference)
      .then((next) => {
        useEditorStore.getState().setEnvironment(next);
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
    if (!name) {
      return;
    }
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
  const query = search.trim().toLowerCase();

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

      <EnvironmentBody
        env={environment}
        defaults={defaults}
        query={query}
        viewState={viewState}
        setViewState={setViewState}
        onCustomized={() => setActiveProfileKey("custom")}
      />

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
              if (event.key === "Enter") {
                saveProfile();
              }
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
    </div>
  );
}
