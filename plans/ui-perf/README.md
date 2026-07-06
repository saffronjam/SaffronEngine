# Editor UI performance — overlay-open latency & document-wide style recalc

**Status:** CODE COMPLETE across all 7 phases — pending (a) user runtime verification of the numbers +
the `Select` rewrite, and (b) a pre-existing/in-flight `src-tauri` fmt+clippy cleanup that is not this
work's to fix. See each `phase-N` file. The fix, in one line: the ~250–500 ms overlay-open stall was a
whole-document style recalc forced by `react-remove-scroll`'s body-level `--removed-body-scroll-bar-size`
custom-property write on the never-unmounted editor DOM — removed by making the app's overlays non-modal
(the `Select` rebuilt on a non-modal Popover; menus default `modal={false}`), with CSS containment +
root `overflow:hidden` + an in-memory image cache + a shared per-grid tooltip as supporting fixes.

**Root-cause refinement during implementation:** the dominant cost is the **custom-property
write**, not `hideOthers` or the body-overflow mutation (those are cheaper / neutralized by
`overflow:hidden`). That makes **Phase 4** the load-bearing fix and reframes Phases 3/5 as
secondary (see their files) — `display:none` already elides hidden subtrees, so post-Phase-4 an open
recalcs only the mounting popover.

Opening *any* Radix overlay (a `Select`, `DropdownMenu`, `Tooltip`, `ContextMenu`, `Dialog`) stalls the
webview for **250–500 ms** before the popup paints. It is worst on the **Store** page (largest visible
DOM) but is **app-wide** — the cost is paid wherever an overlay opens, the Store just makes it most
visible. This planset fixes it structurally, everywhere.

This is not a Store bug, not a React re-render bug, not a network bug, and not StrictMode. It is
**native style-recalc + layout over a never-unmounted DOM**, triggered by Radix's modal overlay
machinery. Everything below is grounded in a real WebKit Web Inspector capture plus a multi-agent code
+ literature audit; see **Diagnosis** for the evidence and **Ruled out** so we don't relitigate.

## Diagnosis (measured, not guessed)

A single click that opens the provider `Select` was captured in the WebKit Inspector Timeline as one
`pointerdown` handler costing **275 ms**, broken down as:

| Work inside the one pointerdown | Time |
|---|---|
| Styles Recalculated ×3 | 60.6 + 60.0 + 46.3 ms (**≈167 ms**) |
| Forced Layout ×2 | 28.0 + 27.2 ms (**≈55 ms**) |
| Focus / MutationObserver / JS | < 2 ms |

So **~222 ms of ~275 ms is the browser's style + layout engine**, not our JavaScript, not React. Our
own handlers (`onOpenChange`, focus) are sub-millisecond. The measured cost also **scales with mounted
DOM**: open→paint rose 250 ms → 450 ms as the Store grid's mounted card count went 8 → 34.

### Root cause chain

1. **Radix `Select` is modal-only and offers no escape hatch** (`radix-ui` issue #1496 open). On open it
   *unconditionally* runs two whole-document passes:
   - **`react-remove-scroll`** writes `overflow:hidden` + scrollbar-gap padding to `<body>` and does a
     synchronous `getComputedStyle(body)` / `clientWidth` read. A `<body>`-box mutation invalidates
     style + layout for the **entire** document → a forced full-document reflow.
   - **`aria-hidden` / `hideOthers`** walks the **whole document**, stamping `aria-hidden` on every
     non-ancestor node (reverting on close). Cost is **O(total mounted nodes)**.
   Together these produce the 3× recalc + 2× forced layout seen in the trace.
2. **The editor never unmounts anything.** Every dock panel body stays mounted and is toggled with
   `display:none` (`components/dock/DockPanelsHost.tsx` `LeafBody`), and every `ViewTab` workspace — the
   Store tab, the asset editor — stays mounted hidden (`app/App.tsx:354,372,388`). `display:none` does
   **not** exempt a subtree from the `hideOthers` attribute walk or from a style-recalc cascade. So both
   passes above traverse the *entire* editor DOM, and each full recalc costs ~60 ms. This is the fixed
   base **and** the scaling term.
3. **Discriminator that pins the cause:** the provider picker already uses `position="popper"`
   (`storefront/StoreWorkspace.tsx`) and the resolution picker uses `item-aligned`
   (`components/ui/select.tsx:48` default), yet **both are equally slow**. The only cost they share is
   the modal scroll-lock + `hideOthers` — so *that* is the cause, not popper positioning.
4. **Network burst = downstream symptom (not the cause).** WebKit custom-scheme resources
   (`saffron-img://`, `storefront/cachedImage.ts`) bypass the resource cache and ignore `Cache-Control`,
   so the `immutable` header at `src-tauri/src/lib.rs:1273` is inert. When the full-document repaint
   re-touches the visible `loading="lazy"` thumbnails, WebKit **re-invokes the Rust scheme handler** per
   image — the blue "Network Requests" burst. `cachedImage()` is deterministic and the grid does zero
   re-renders on open, so React is not re-issuing these; it is WebKit re-serving an uncached resource
   after the native repaint.

### The two independent levers

- **Make a full recalc cheap** — stop a `<body>`-level invalidation from cascading into (and relaying
  out) huge hidden subtrees. CSS containment + hidden-subtree elision. *Attacks the ~60 ms-per-pass.*
- **Stop opening overlays from doing whole-document work** — take the app's overlays off the modal
  scroll-lock + `hideOthers` path. *Attacks "why 3 recalcs + 2 layouts at all."*

Either lever alone helps; both together is the thorough fix. We do the cheap containment levers first
(they may prove sufficient), re-measure, then go as deep on the overlay refactor as the numbers demand.

## Phases (dependency-ordered)

| Phase | Title | Lever | Risk | Depends on |
|---|---|---|---|---|
| [1](phase-1-measurement-harness.md) | Measurement harness & baseline | — | low | — |
| [2](phase-2-css-containment.md) | CSS containment + scroll-lock no-op | cheap recalc | low | 1 |
| [3](phase-3-hidden-subtree-elision.md) | Elide hidden tabs/panels from recalc & a11y walk | cheap recalc | medium | 1, 2 |
| [4](phase-4-nonmodal-overlays.md) | Non-modal overlay primitives, app-wide | no whole-doc work | high | 1, 3 |
| [5](phase-5-dom-weight.md) | Reduce always-mounted DOM weight | smaller O(nodes) | medium | 1, 3 |
| [6](phase-6-image-scheme-cache.md) | Cache `saffron-img://` so repaint doesn't re-request | kills network burst | medium | 1 |
| [7](phase-7-verify-docs.md) | Re-measure, remove instrumentation, document | gate | low | all |

Phases 2, 3, 6 are independent of each other and of 4/5 — they can land in any order once Phase 1
exists. Phase 4 is the deep structural change and should follow Phase 3 so we know how much of the cost
the cheap levers already removed (Phase 4 may shrink to Select-only if 2+3 handle the menus/tooltips).

## Success criteria (the gate for Phase 7)

- Opening **any** overlay (Select, DropdownMenu, Tooltip, ContextMenu, Dialog) on the **Store** page
  with a full grid: **< 32 ms** open→paint (≈2 frames), down from 250–500 ms.
- The same overlays on the **Scene** page: **< 16 ms**.
- **No** `saffron-img://` re-request burst when an overlay opens over the grid.
- Open→paint no longer scales with mounted card count.
- `just prepare-for-commit` clean (cargo fmt + clippy `-D warnings` + oxlint + oxfmt); `bun run check`
  and `vite build` green. No behavior regressions (tabs/panels keep state across switches; dialogs stay
  non-modal and scoped per the Store dialog rules in `storefront/AGENTS.md`).

## Ruled out (do not relitigate)

- **React StrictMode / dev double-effects** — disabling `<StrictMode>` in `main.tsx` changed the timing
  by nothing. Not the cause.
- **Heavy interactive-card accumulation as the *primary* cause** — instrumented `heavyCards` count has
  near-zero correlation with open→paint (27 cards was *faster* than 10 in one capture). It is at most a
  minor contributor to the O(nodes) term, folded into Phase 5.
- **Unstable image URLs / React re-issuing fetches** — `cachedImage()` is pure and deterministic and
  the grid does not re-render on open. The burst is WebKit re-serving an uncached custom-scheme
  resource after the native repaint (Phase 6), not a React bug.
- **Software (llvmpipe) webview path** — the capture was on the hardware path; the cost is CPU-side
  style/layout, independent of the GPU compositor.
- **A wired fetch/invoke on the open handler** — there is none; the only `onOpenChange` code is the
  temporary perf instrumentation.

## Coordination note

Store files (`storefront/StoreResultsGrid.tsx`, `StoreWorkspace.tsx`, `ImportControls.tsx`,
`AssetDetailModal.tsx`, `ProviderModal.tsx`, `dialog.tsx`, `storeOverlay.ts`) are under **active edit
for the non-modal dialog work**. Any phase touching them must **re-read immediately before editing** and
rebase onto the landed dialog changes. The scoped-overlay pattern that work introduced
(`storeOverlay.ts` + `useStoreOverlayContainer` + `DialogScopedOverlay` + `modal={false}`) is the
**proven template** Phase 4 generalizes — extend it, do not reinvent it, and never revert the Store
dialogs to body-portaled modal `Dialog`s.

## Temporary instrumentation to remove (Phase 7)

Currently in the tree from the investigation, all marked `TEMP … DELETE with perfDebug.ts`:
`storefront/perfDebug.ts` (new file); `logRender`/`noteInteractive`/`onOpenChange` calls in
`ImportControls.tsx`, `StoreResultsGrid.tsx`, `StoreWorkspace.tsx`; and the disabled `<StrictMode>` in
`main.tsx` (**restore it**). Phase 1 replaces these ad-hoc hacks with a proper dev-gated harness; Phase 7
deletes everything.
