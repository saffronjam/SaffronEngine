# panels — the editor's dockable panel bodies

Every dockable panel body plus the helper modules a few of them need. Read `editor/AGENTS.md` first:
its "Rules that are easy to break" bind here in full, and this file only adds what is specific to
panels. Vegetation is the largest area — eleven registered panels plus their helper modules — and
its engine-side contracts live in `engine/crates/vegetation/AGENTS.md`.

## Registration and islands

A panel body is not reachable until it is registered. Three places, all in one change:

- `components/dock/panelRegistry.tsx` — the id, title, closability, group, and renderer;
- `state/dockLayout.ts` — membership in `SCENE_PANEL_IDS` or `ASSET_EDITOR_PANEL_IDS`, and a
  `DEFAULT_LEAF` entry naming where it first appears;
- the body itself, here.

The two dockspace islands are disjoint and stay that way — a scene panel and an asset-editor panel
share no ids. Vegetation spans both: `vegetation`, `ecologyTimeline`, and `vegetationTelemetry` are
scene panels; `vegSummary`, `plantGraph`, `plantWind`, `plantAtlas`, `plantHierarchy`,
`plantSeason`, `plantProxies`, and `biomeGraph` live in the asset-editor island.

## Rules that are easy to break

- **Panel bodies render once at the app root and are re-parented, never remounted.** `LeafBody` in
  `components/dock/DockPanelsHost.tsx` moves the rendered node into its leaf with `appendChild`.
  Rendering a body inside the leaf tree makes a dock move destroy its state, its refs, and the live
  viewport surface.
- **One error location.** Every `catch` on a control call ends in `notifyError(errorText(err))` from
  `lib/flash.ts`. No local `useState<string|null>` error banner, no inline destructive strip, no
  bare `console.error`, no `alert`. A silently swallowed `catch` is a bug — the user must see why an
  action did nothing. `notify(...)` is for a non-error result toast.
- **Tooltips are the Radix primitive, never `title=`.** A native `title` attribute renders as an
  unstyled browser tooltip. And a tooltip must *add* information: an ambiguous icon button, a
  shortcut, or why a control is disabled — never a repeat of visible text.
- **A large list re-renders only the rows that changed.** Memoized rows subscribing to their own
  derived primitive, referentially stable props, and one shared context menu per surface rather than
  a Radix root per row. Verify with the dev-mode `logRender` counters.
- **Every mutating action records its inverse** via `pushEdit`. Undo is editor-only, reconstructed
  from paired control calls; an action with no `pushEdit` is silently un-undoable. Vegetation is the
  exception to the composing half: `vegetationGesture.ts` applies a batch and pushes the inverse the
  engine replies with, which flips each time it is applied, so no panel reconstructs a preimage.
- **Panel surfaces use the semantic theme tokens** (`bg-background`, `bg-card`, `text-foreground`,
  `border-border`), never raw `neutral-*`.
- **Ids are strings end-to-end.** Entity ids are u64 and `PlantId` is u128 in the engine. Never
  `Number()` either one.

## Vegetation panels

| File | Role |
|---|---|
| `VegetationPanel.tsx` | The main scene panel: layers, brushes, cook and evaluation state |
| `VegetationViewportToolbar.tsx`, `vegetationTools.ts` | The viewport tool strip and its tool vocabulary |
| `vegetationPainting.ts`, `vegetationPlanting.ts` | The brush interaction model and single-plant placement |
| `vegetationShapes.ts` | The Volume and Spline gestures, which write a layer's analytic operator |
| `EcologyTimelinePanel.tsx` | Step and run over the world's biological clock |
| `VegetationTelemetryPanel.tsx` | The running world's vegetation counters and budgets |
| `PlantGraphPanel/` | The botanical graph on a node canvas, its operator parameters, the structure it grows, and validation |
| `PlantWindPanel.tsx` | Wind and interaction preview: drives the real fields the previewed plant stands in |
| `PlantAtlasPanel.tsx` | The packed coverage atlas the family samples, with a scrubbable mip level |
| `PlantHierarchyPanel.tsx` | The cooked cut and its declared errors, with the cut pinnable so the preview draws either representation |
| `PlantSeasonPanel.tsx` | Scrubs the year and binds the appearance the engine resolves to the live preview |
| `PlantProxiesPanel.tsx` | Draws the family's derived collision capsules and navigation footprints over the preview |
| `BiomeGraphPanel.tsx` | The biome placement graph |
| `VegetationAssetWorkspace.tsx`, `vegetationAssetDetails.ts` | The asset-editor workspace for `.splant` / `.sbiome` / `.svegmap` |

- **The ecology timeline is step-and-run, never a seek bar.** Biological time only moves forward by
  executing ticks — there is no analytical fast-forward to scrub against, so a slider would promise
  something the simulation cannot do. A long run is chunked so catch-up stays interruptible, and the
  running flag is a `ref` rather than state because a stale closure would keep stepping after the
  user pressed pause.
- **A region only advances while every cell it spans is resident**, so the timeline separates
  "caught up" from "waiting on residency". Those are different problems and must not collapse into
  one status.
- **A native family's structure is derived, so the graph panel does not edit it.** Parts,
  dimensions, spines, phenotypes, and proxies all come out of growing the graph. The panel edits the
  graph document, its module bindings, and the variation list, and reads everything else back. One
  recorded edit is one semantic operation, and its inverse is the previous document replayed through
  the same single write path — nothing reconstructs old parts by hand.
- **A graph write must not carry derived fields forward.** `plant-graph-set` sends the graph, the
  family's own grafts, and its module bindings; sending `variations`, `phenotypes`,
  `collision_proxies`, or `navigation_proxies` back creates a second truth about geometry the engine
  is about to re-derive.
- **A module call and its binding move together.** The family validator checks both directions — a
  `module-call` node whose GUID names no reference, or a reference no node calls, is refused — so
  adding one mints the call GUID, the node, and the reference in a single document edit, and
  removing one takes both out. The add palette therefore does not offer a bare `module-call`.
- **A parameter scrub is one edit, not one per keystroke.** Field changes edit a local document and
  write once the typing settles; a structural gesture that lands mid-burst folds the burst into
  itself rather than recording a second entry. Writes queue on one promise chain so a later reply
  can never publish an older document.
- **An imported family has no botanical graph.** That is a fact about the asset, not a failed
  operation, so the panel says so in place rather than raising a toast.
- **Vegetation shortcuts are scoped to the open panel.** `CommandScope` has a dedicated
  `"vegetation"` scope and `app/useVegetationShortcuts.ts` binds it; with the panel closed the digit
  keys pass through untouched so the mode never steals keys from ordinary editing. Register a
  shortcut as a command in `lib/keybindings.ts` — never compare `e.key` inline.

## Debugging what you cannot see

An agent has no view of the running editor. Do not ship a fix for a visual or timing bug on a
hypothesis: add temporary `[vp-dbg]`-prefixed logging that reaches the `just run` terminal, ask the
user to restart (Vite HMR does not reliably apply store-shape changes), have them paste the output,
then diagnose and remove the logging. A bug is not fixed until the user confirms it against real
output.
