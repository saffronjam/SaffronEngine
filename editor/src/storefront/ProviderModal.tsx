// The provider setup modal: a grid of provider logos (left) with a detail/auth pane that
// smoothly extends out to the right when a provider is selected. Selecting a tile never enables
// it — enabling happens in the detail pane, gated on any credential the provider needs. Opens
// automatically on first use (nothing enabled) and from the Store's gear button.
import { useEffect, useState } from "react";
import { invoke } from "../shell";
import { Check } from "lucide-react";

import { Button } from "@/components/ui/button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogScopedOverlay,
  DialogTitle,
} from "@/components/ui/dialog";
import { cn } from "@/lib/utils";

import { ApiKeyField } from "./ApiKeyField";
import { ProviderLogo } from "./connectorIcons";
import { errorText, notifyError } from "../lib/flash";
import { useStoreOverlayContainer } from "./storeOverlay";
import { connectorLogin, connectorSecretStatus, type ConnectorInfo } from "./types";

function prettyHost(url: string): string {
  return url.replace(/^https?:\/\//, "").replace(/\/$/, "");
}

function openExternal(url: string) {
  void invoke("open_external", { url }).catch((err: unknown) => notifyError(errorText(err)));
}

export function ProviderModal({
  open,
  onOpenChange,
  connectors,
  enabled,
  onEnabledChange,
}: {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  connectors: ConnectorInfo[];
  enabled: string[];
  onEnabledChange: (next: string[]) => void;
}) {
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const container = useStoreOverlayContainer();

  // Each fresh open starts grid-only (centered); no stale selection extends the modal.
  useEffect(() => {
    if (!open) setSelectedId(null);
  }, [open]);

  const selected = connectors.find((c) => c.id === selectedId) ?? null;
  const isEnabled = (id: string) => enabled.includes(id);

  const enable = (id: string) => {
    if (!enabled.includes(id)) onEnabledChange([...enabled, id]);
  };
  const disable = (id: string) => onEnabledChange(enabled.filter((e) => e !== id));

  // The first-provider modal cannot be dismissed until at least one provider is enabled —
  // closing the Store tab is the only way out before then.
  const canClose = enabled.length > 0;
  const handleOpenChange = (next: boolean) => {
    if (!next && !canClose) return;
    onOpenChange(next);
  };

  // Scoped to the Store view's region; hold off until that container exists so we never briefly
  // portal to document.body.
  const visible = open && container != null;

  return (
    <Dialog open={visible} onOpenChange={handleOpenChange} modal={false}>
      <DialogScopedOverlay
        open={visible}
        container={container}
        onClose={canClose ? () => onOpenChange(false) : undefined}
      />
      <DialogContent
        container={container}
        showCloseButton={canClose}
        onEscapeKeyDown={(e) => {
          if (!canClose) e.preventDefault();
        }}
        onInteractOutside={(e) => e.preventDefault()}
        onPointerDownOutside={(e) => e.preventDefault()}
        className="absolute flex h-[560px] w-auto max-w-[calc(100%-2rem)] gap-0 overflow-hidden p-0 sm:max-w-[940px]"
      >
        <div className="flex w-[520px] shrink-0 flex-col p-6">
          <DialogTitle>
            {enabled.length === 0 ? "Add your first provider" : "Manage providers"}
          </DialogTitle>
          <DialogDescription className="mt-1 text-xs">
            Enable at least one provider to browse assets.
          </DialogDescription>
          <div
            className="mt-5 grid gap-3"
            style={{ gridTemplateColumns: `repeat(${connectors.length || 1}, minmax(0, 1fr))` }}
          >
            {connectors.map((c) => (
              <button
                key={c.id}
                type="button"
                onClick={() => setSelectedId(c.id)}
                className={cn(
                  "group flex cursor-pointer flex-col items-center gap-2 rounded-lg border border-transparent p-3",
                  "transition-colors hover:border-ring hover:bg-accent/40",
                  selectedId === c.id && "border-ring bg-accent/60 ring-1 ring-ring",
                )}
              >
                <div className="relative flex aspect-square w-full items-center justify-center p-1.5">
                  <ProviderLogo connectorId={c.id} name={c.displayName} className="size-full" />
                  {isEnabled(c.id) ? (
                    <span className="absolute top-0 right-0 rounded-full bg-emerald-500 p-0.5">
                      <Check className="size-3 text-white" />
                    </span>
                  ) : null}
                </div>
                <span className="w-full truncate text-center text-xs font-medium text-foreground">
                  {c.displayName}
                </span>
              </button>
            ))}
          </div>
        </div>

        <div
          className={cn(
            "h-full shrink-0 overflow-hidden bg-card transition-[width] duration-200 ease-out",
            selected && "border-l border-border",
          )}
          style={{ width: selected ? 420 : 0 }}
        >
          {selected ? (
            <DetailPane
              connector={selected}
              enabled={isEnabled(selected.id)}
              onCancel={() => setSelectedId(null)}
              onEnable={() => enable(selected.id)}
              onDisable={() => disable(selected.id)}
            />
          ) : null}
        </div>
      </DialogContent>
    </Dialog>
  );
}

function DetailPane({
  connector,
  enabled,
  onCancel,
  onEnable,
  onDisable,
}: {
  connector: ConnectorInfo;
  enabled: boolean;
  onCancel: () => void;
  onEnable: () => void;
  onDisable: () => void;
}) {
  const needsSecret = connector.authKind !== "none";
  const [hasSecret, setHasSecret] = useState(!needsSecret);
  const [busy, setBusy] = useState(false);

  useEffect(() => {
    if (!needsSecret) {
      setHasSecret(true);
      return;
    }
    connectorSecretStatus(connector.id)
      .then(setHasSecret)
      .catch(() => {});
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [connector.id]);

  const authenticate = () => {
    setBusy(true);
    connectorLogin(connector.id)
      .then(() => setHasSecret(true))
      .catch((err: unknown) => notifyError(errorText(err)))
      .finally(() => setBusy(false));
  };

  return (
    <div className="flex h-full w-[420px] flex-col p-6">
      <div className="flex items-center gap-3">
        <ProviderLogo
          connectorId={connector.id}
          name={connector.displayName}
          className="size-14 shrink-0"
        />
        <div className="min-w-0">
          <div className="truncate text-sm font-medium text-foreground">
            {connector.displayName}
          </div>
          {connector.website ? (
            <button
              type="button"
              className="truncate text-xs text-muted-foreground hover:text-foreground hover:underline"
              onClick={() => openExternal(connector.website)}
            >
              {prettyHost(connector.website)}
            </button>
          ) : null}
        </div>
      </div>

      {connector.description ? (
        <p className="mt-3 text-xs leading-relaxed text-muted-foreground">
          {connector.description}
        </p>
      ) : null}

      {connector.authKind === "apiKey" && !enabled ? (
        <div className="mt-4">
          <p className="mb-1.5 text-[11px] text-muted-foreground">Stored only on this machine.</p>
          <ApiKeyField
            connectorId={connector.id}
            displayName={connector.displayName}
            onStatusChange={setHasSecret}
          />
        </div>
      ) : null}

      <div className="mt-auto flex items-center justify-end gap-2 pt-4">
        <Button type="button" variant="ghost" size="sm" onClick={onCancel}>
          Cancel
        </Button>
        {connector.authKind === "oauthLoopback" && !enabled && !hasSecret ? (
          <Button type="button" size="sm" disabled={busy} onClick={authenticate}>
            {busy ? "Authenticating…" : "Authenticate"}
          </Button>
        ) : null}
        {enabled ? (
          <Button type="button" size="sm" variant="destructive" onClick={onDisable}>
            Disable
          </Button>
        ) : (
          <Button type="button" size="sm" disabled={!hasSecret} onClick={onEnable}>
            Enable
          </Button>
        )}
      </div>
    </div>
  );
}
