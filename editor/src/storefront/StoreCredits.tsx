// The credits view: every imported asset whose license requires attribution, read off the
// catalog (where import recorded it). This is the project's one-stop credits surface for
// CC-BY / Sketchfab content — author, license, source, and the originating store.
import { useEffect, useState } from "react";
import { invoke } from "../shell";

import { Badge } from "@/components/ui/badge";

import { client } from "../control/client";
import { errorText, notifyError } from "../lib/flash";

type Asset = Awaited<ReturnType<typeof client.listAssets>>["assets"][number];

export function StoreCredits() {
  const [assets, setAssets] = useState<Asset[]>([]);

  useEffect(() => {
    client
      .listAssets()
      .then((list) => setAssets(list.assets))
      .catch((err: unknown) => notifyError(errorText(err)));
  }, []);

  const attributed = assets.filter((a) => a.attribution?.requiresAttribution);

  return (
    <div className="min-h-0 flex-1 overflow-auto p-4">
      <h2 className="mb-1 text-sm font-medium text-foreground">Credits</h2>
      <p className="mb-4 max-w-2xl text-xs text-muted-foreground">
        Assets whose license requires attribution. Keep these credits with your game.
      </p>
      {attributed.length === 0 ? (
        <p className="text-xs text-muted-foreground italic">
          No imported assets require attribution yet.
        </p>
      ) : (
        <div className="flex max-w-2xl flex-col gap-2">
          {attributed.map((a) => {
            const attr = a.attribution!;
            return (
              <div key={a.id} className="rounded-md border border-border bg-card p-3">
                <div className="flex items-center gap-2">
                  <span className="text-sm font-medium text-foreground">{a.name}</span>
                  <Badge variant="outline" className="text-[10px] uppercase">
                    {attr.licenseId}
                  </Badge>
                  {attr.storeId ? (
                    <Badge variant="secondary" className="text-[10px]">
                      {attr.storeId}
                    </Badge>
                  ) : null}
                </div>
                <div className="mt-1 text-xs text-muted-foreground">
                  by {attr.author || "unknown"}
                  {attr.sourceUrl ? (
                    <>
                      {" · "}
                      <button
                        type="button"
                        className="hover:text-foreground hover:underline"
                        onClick={() =>
                          void invoke("open_external", { url: attr.sourceUrl }).catch(
                            (err: unknown) => notifyError(errorText(err)),
                          )
                        }
                      >
                        view source
                      </button>
                    </>
                  ) : null}
                </div>
              </div>
            );
          })}
        </div>
      )}
    </div>
  );
}
