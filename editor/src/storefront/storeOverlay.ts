// The Store view's overlay container. The detail and provider dialogs are non-modal and scoped to
// the Store workspace region (below the main tab strip) rather than the whole window, so they portal
// into this element instead of document.body. StoreWorkspace publishes its root here; the deeply
// nested AssetDetailModal (inside the virtualized grid) and the ProviderModal read it.
import { createContext, useContext } from "react";

const StoreOverlayContext = createContext<HTMLElement | null>(null);

export const StoreOverlayProvider = StoreOverlayContext.Provider;

/// The element scoped store dialogs portal into (null before the Store view has mounted).
export function useStoreOverlayContainer(): HTMLElement | null {
  return useContext(StoreOverlayContext);
}
