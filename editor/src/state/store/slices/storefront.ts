import {
  loadStoreKind,
  loadStoreSearchText,
  loadStoreSelected,
  persistStoreQuery,
  persistStoreSelected,
} from "../persistence";
import type { SetEditorState, StorefrontSlice } from "../types";

export function createStorefrontSlice(set: SetEditorState): StorefrontSlice {
  return {
    storeSelected: loadStoreSelected(),
    storeSearchText: loadStoreSearchText(),
    storeKind: loadStoreKind(),
    storeSession: null,
    storeResults: [],
    storeExhausted: false,
    storeScrollTop: 0,
    storeResultsSession: null,

    setStoreSelected: (storeSelected) => {
      persistStoreSelected(storeSelected);
      set({ storeSelected });
    },
    setStoreQuery: ({ text, kind }) => {
      persistStoreQuery(text, kind);
      set({ storeSearchText: text, storeKind: kind });
    },
    setStoreSession: (storeSession) => set({ storeSession }),
    setStoreResults: (storeResults, session) => set({ storeResults, storeResultsSession: session }),
    appendStoreResults: (results, session) =>
      set((s) => ({ storeResults: [...s.storeResults, ...results], storeResultsSession: session })),
    setStoreExhausted: (storeExhausted) => set({ storeExhausted }),
    setStoreScrollTop: (storeScrollTop) => set({ storeScrollTop }),
    resetStoreBrowse: () =>
      set({
        storeSession: null,
        storeResults: [],
        storeResultsSession: null,
        storeExhausted: false,
        storeScrollTop: 0,
      }),
  };
}
