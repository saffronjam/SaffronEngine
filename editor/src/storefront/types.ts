// The Store connector surface lives in the shell (editor-local `store_*`/`connector_*` commands), not the
// engine control plane — these mirror the Rust `connectors` module's camelCase wire types.
// Only the eventual *import* crosses to the host (via the typed control client).
import { Channel, invoke } from "../shell";

export type AuthKind = "none" | "apiKey" | "oauthLoopback";
export type StoreKind = "model" | "hdri" | "material" | "texture";

export interface StoreLicense {
  id: string;
  requiresAttribution: boolean;
  url: string;
}

export interface StoreRef {
  id: string;
  displayName: string;
}

export interface StoreImportDescriptor {
  format: string;
  ref: string;
}

export interface StoreResult {
  id: string;
  store: StoreRef;
  kind: StoreKind;
  name: string;
  author: string;
  thumbnailUrl: string;
  sourceUrl: string;
  license: StoreLicense;
  importDescriptor: StoreImportDescriptor;
  hasParts: boolean;
  supportsResolution: boolean;
  publishedAt?: string;
  updatedAt?: string;
  triCount?: number;
  fileSize?: number;
  resolution?: string;
  tags?: string[];
}

export interface ConnectorInfo {
  id: string;
  displayName: string;
  authKind: AuthKind;
  description: string;
  website: string;
  enabled: boolean;
}

export interface SearchQuery {
  text: string;
  kind?: StoreKind;
  /// The single connector id the search runs against (chosen in the Store's store dropdown).
  provider: string;
}

export interface SearchMore {
  results: StoreResult[];
  exhausted: boolean;
}

export interface ImportedAsset {
  id: string;
  name: string;
}

// One selectable constituent of an asset (a single map, etc.). `ref`/`bundle` are opaque
// connector download handles echoed back to the bridge for download_part.
export interface AssetPart {
  id: string;
  label: string;
  importKind: StoreKind;
  role?: string;
  resolution?: string;
  format?: string;
  size?: number;
  ref: string;
  bundle?: string;
}

export function storeListConnectors(): Promise<ConnectorInfo[]> {
  return invoke<ConnectorInfo[]>("store_list_connectors");
}

export function storeSearchSession(query: SearchQuery): Promise<string> {
  return invoke<string>("store_search_session", { query });
}

export function storeSearchMore(session: string, count: number): Promise<SearchMore> {
  return invoke<SearchMore>("store_search_more", { session, count });
}

export function storeImport(
  result: StoreResult,
  resolution?: string,
  onProgress?: (fraction: number) => void,
): Promise<ImportedAsset> {
  // The host streams the download fraction (0–1) back over this channel.
  const channel = new Channel<number>();
  if (onProgress) channel.onmessage = onProgress;
  return invoke<ImportedAsset>("store_import", {
    result,
    resolution: resolution ?? null,
    onProgress: channel,
  });
}

export function storeAssetParts(result: StoreResult): Promise<AssetPart[]> {
  return invoke<AssetPart[]>("store_asset_parts", { result });
}

// One preview image of an asset (a render or a constituent map), shown in the gallery.
// `url` is card-sized; `fullUrl` (when present) is a higher-res variant for the detail view.
export interface GalleryImage {
  url: string;
  label?: string;
  fullUrl?: string;
}

export function storeAssetGallery(result: StoreResult): Promise<GalleryImage[]> {
  return invoke<GalleryImage[]>("store_asset_gallery", { result });
}

export function storeImportPart(
  result: StoreResult,
  part: AssetPart,
  resolution?: string,
): Promise<ImportedAsset> {
  return invoke<ImportedAsset>("store_import_part", {
    result,
    part,
    resolution: resolution ?? null,
  });
}

// Credential commands — the secret never returns to the webview, only a presence boolean.
export function connectorSetSecret(connectorId: string, secret: string): Promise<void> {
  return invoke<void>("connector_set_secret", { connectorId, secret });
}

export function connectorClearSecret(connectorId: string): Promise<void> {
  return invoke<void>("connector_clear_secret", { connectorId });
}

export function connectorSecretStatus(connectorId: string): Promise<boolean> {
  return invoke<boolean>("connector_secret_status", { connectorId });
}

// Runs an oauth_loopback connector's browser login; resolves once a token is stored.
export function connectorLogin(connectorId: string): Promise<void> {
  return invoke<void>("connector_login", { connectorId });
}
