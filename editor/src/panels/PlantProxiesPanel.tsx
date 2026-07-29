/// The Plant workspace's collision and navigation view.
///
/// The surface is the preview itself: the toggles here draw the family's derived capsules and
/// footprints over the plant, through the same overlay the scene uses for physics colliders. The
/// table beside them is the legend, not the view — a list of half-extents tells you nothing about
/// whether a capsule actually wraps the trunk it was fitted to.
///
/// Proxies are DERIVED, like the dimensions and the spines: they are a result of what grew, so there
/// is nothing here to edit. What an author checks is whether the result is right, which is a
/// question only the drawing answers.
import { useCallback, useEffect, useState } from "react";
import { client } from "../control/client";
import { errorText, notifyError } from "../lib/flash";
import type { CommandResultMap } from "../protocol";
import { Button } from "@/components/ui/button";
import { Label } from "@/components/ui/label";
import { ScrollArea } from "@/components/ui/scroll-area";
import { Separator } from "@/components/ui/separator";
import { useEditorStore } from "../state/store";

type Proxies = CommandResultMap["plant-proxies"];

export function PlantProxiesPanel() {
  const selectedAssets = useEditorStore((state) => state.selectedAssetIds);
  const subject = selectedAssets.size === 1 ? [...selectedAssets][0]! : null;
  const [proxies, setProxies] = useState<Proxies | null>(null);
  const [collision, setCollision] = useState(false);
  const [navigation, setNavigation] = useState(false);
  const [unavailable, setUnavailable] = useState<string | null>(null);

  const load = useCallback(async () => {
    if (!subject) {
      setProxies(null);
      return;
    }
    try {
      setProxies(await client.plantProxies({ plant: subject }));
      setUnavailable(null);
    } catch (err) {
      setProxies(null);
      setUnavailable(errorText(err));
    }
  }, [subject]);

  useEffect(() => {
    void load();
    void client
      .getDebugOverlays()
      .then((overlays) => {
        setCollision(overlays.colliders);
        setNavigation(overlays.vegetationNavigation);
      })
      .catch(() => {});
  }, [load]);

  const toggle = useCallback(
    async (patch: { colliders?: boolean; vegetationNavigation?: boolean }) => {
      try {
        const next = await client.setDebugOverlays(patch);
        setCollision(next.colliders);
        setNavigation(next.vegetationNavigation);
      } catch (err) {
        notifyError(errorText(err));
      }
    },
    [],
  );

  return (
    <ScrollArea className="h-full">
      <div className="flex flex-col gap-3 p-3">
        <section className="flex flex-col gap-2">
          <Label className="text-[11px] text-muted-foreground">Draw over the preview</Label>
          <div className="flex flex-wrap gap-1">
            <Button
              className="h-7 text-[11px]"
              onClick={() => void toggle({ colliders: !collision })}
              size="sm"
              variant={collision ? "default" : "outline"}
            >
              Collision
            </Button>
            <Button
              className="h-7 text-[11px]"
              onClick={() => void toggle({ vegetationNavigation: !navigation })}
              size="sm"
              variant={navigation ? "default" : "outline"}
            >
              Navigation
            </Button>
          </div>
        </section>

        <Separator />

        {unavailable ? <p className="text-[11px] text-muted-foreground">{unavailable}</p> : null}

        {proxies ? (
          <>
            <section className="flex flex-col gap-1">
              <h3 className="text-[11px] font-medium text-foreground">
                Collision{" "}
                <span className="text-muted-foreground">({proxies.collision.length})</span>
              </h3>
              {proxies.collision.map((proxy, index) => (
                <div className="flex items-baseline justify-between gap-2" key={index}>
                  <span className="text-[11px] text-muted-foreground">
                    {proxy.shape}
                    {proxy.breakable ? " · breakable" : ""}
                  </span>
                  <span className="font-mono text-[11px] tabular-nums text-foreground">
                    {proxy.dimensionsM.map((value) => value.toFixed(2)).join(" × ")} m
                  </span>
                </div>
              ))}
              {proxies.collision.length === 0 ? (
                <p className="text-[10px] text-muted-foreground">
                  None derived — nothing here is thick enough for a character to collide with.
                </p>
              ) : null}
            </section>

            <section className="flex flex-col gap-1">
              <h3 className="text-[11px] font-medium text-foreground">
                Navigation{" "}
                <span className="text-muted-foreground">({proxies.navigation.length})</span>
              </h3>
              {proxies.navigation.map((proxy, index) => (
                <div className="flex items-baseline justify-between gap-2" key={index}>
                  <span className="text-[11px] text-muted-foreground">
                    {proxy.footprintM.length} points
                  </span>
                  <span className="font-mono text-[11px] tabular-nums text-foreground">
                    {proxy.heightM.toFixed(2)} m · cost {proxy.cost.toFixed(2)}
                  </span>
                </div>
              ))}
              {proxies.navigation.length === 0 ? (
                <p className="text-[10px] text-muted-foreground">
                  None derived — nothing here obstructs a path.
                </p>
              ) : null}
            </section>
          </>
        ) : null}

        <Button
          className="h-7 self-start text-[11px]"
          onClick={() => void load()}
          size="sm"
          variant="outline"
        >
          Refresh
        </Button>
      </div>
    </ScrollArea>
  );
}
