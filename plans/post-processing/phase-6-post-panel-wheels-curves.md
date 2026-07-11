# Phase 6 — Editor Post panel: color wheels + tone-curve widgets

**Status:** COMPLETED

Part of `plans/post-processing/` (bloom + color grading as one pre-tonemap post subsystem). This is the sixth and final phase, and the only editor-native one — no engine, shader, protocol, or control-plane change lands here (`just engine` is unchanged; the DTOs and commands all shipped in phases 1–5). It builds a dedicated **Post** panel and the two widgets the editor is missing today — a Resolve-style `GradingWheel` trackball and an SVG `ToneCurve` spline — then migrates *every* bloom and grade control off `RenderPanel` into it, deleting the temporary rows phases 1–5 parked there in the same change (NO-LEGACY). It does not block any earlier phase: bloom and grading are already fully functional from the CLI and the interim RenderPanel rows before this phase runs; Phase 6 only re-homes and upgrades the UI.

## Why this phase exists

Phases 1–5 land their controls as plain rows in `RenderPanel.tsx` (`editor/src/panels/RenderPanel.tsx`) because it is the closest template — it already owns the `renderStats` shallow slice, the optimistic fold, and the `pushEdit(...,"scene")` undo (`onExposure`/`onExposureDragStart`/`onExposureDragEnd`). That is deliberate interim scaffolding, not the destination: RenderPanel is anti-aliasing / quality / resolution / tonemap / FPS / feature-toggle config, and stacking bloom + a five-section grade (global + shadows/midtones/highlights) on top of it turns one scroll column into an unusable wall. The grade also *needs* affordances the editor does not have — `react-colorful` in `ColorField.tsx` is a saturation/hue **square**, not a lift/gamma/gain trackball, and there is no curve editor anywhere in `editor/src/components/`. This phase gives grading its own home and its two proper widgets, and returns RenderPanel to exactly what it was.

## Goal

- **A `Post` panel exists as a first-class dockable Scene panel.** One `SCENE_PANEL_REGISTRY` row (`editor/src/components/dock/panelRegistry.tsx`) + one id in `SCENE_PANEL_IDS` / `DEFAULT_LEAF` / the default layout (`editor/src/state/dockLayout.ts`), sectioning **Bloom · Grade Global · Shadows · Midtones · Highlights**, cloning RenderPanel's store-slice + optimistic + per-gesture-undo structure exactly.
- **`GradingWheel.tsx` is the one way to author Lift/Gamma/Gain and per-range CDL.** A hue/sat trackball + vertical luma trim, adopting the same `useScrubValue` + `onDragStart`/`onDragEnd` + `makeCoalescer` contract every field widget uses, driving `set-color-grading` (the Phase 3/4 command). The `VectorEditor` RGB-triplet rows phases 3–4 used as the interim wheel stand-in are **deleted**, not kept beside it.
- **`ToneCurve.tsx` is the one way to author tone curves.** An SVG draggable spline for a master + per-channel (R/G/B) curve, baked into the Phase 5 creative-LUT slot — no parallel curve path.
- **RenderPanel carries zero bloom/grade rows after this phase.** Every migrated row, handler, and import is removed from `RenderPanel.tsx` in the same change (NO-LEGACY cutover); RenderPanel is back to AA/quality/resolution/tonemap/FPS/toggles/debug only.

## NO-LEGACY checklist for this phase

- The interim bloom + grade rows in `RenderPanel.tsx` (the `set-bloom` Switch/`NumberDrag`/`ColorField` rows from Phase 1–2; the grade `NumberDrag` temp/tint/contrast/saturation + `VectorEditor` CDL rows from Phase 3–4; the Phase-5 LUT drop slot) are **gone** tree-wide, along with their `on…`/`record…` handlers and any bloom/grade-only imports. There is no "keep them in Render too, for now" — `PostProcessPanel.tsx` is the sole surface.
- The `VectorEditor`-as-CDL-fallback pattern (three RGB sliders standing in for a wheel, used in the Phase 3–4 interim rows) is **replaced** by `GradingWheel`, not left as a second way to edit the same CDL fields.
- No second command, store field, or undo lane: the Post panel calls the existing `client.setBloom` / `client.setColorGrading` wrappers, reads the same `renderStats` slice (the Phase 1/3 `bloom` / `colorGrading` read-back blocks on `RenderStatsDto`), and records through the same `pushEdit(edit, "scene")` — the `recordRender` shape lifted verbatim from RenderPanel.
- No engine/protocol edit: if a change here needs a new wire field, it belongs in an earlier phase, not this one. This phase must build against the Phase 1–5 contract as-is (`just engine` stays green untouched).

## Panel registration — one row, one id, one leaf

**File `editor/src/components/dock/panelRegistry.tsx`.**

Add a `postProcess` row to `SCENE_PANEL_REGISTRY`, mirroring the `render` row (non-closable, `onlyWhenVisible`), and import the new panel body.

```tsx
import { PostProcessPanel } from "../../panels/PostProcessPanel";

// …inside SCENE_PANEL_REGISTRY, beside `render`:
postProcess: {
  id: "postProcess",
  title: "Post",
  closable: false,
  renderer: "onlyWhenVisible",
  component: PostProcessPanel,
},
```

**File `editor/src/state/dockLayout.ts`.** A non-closable panel must be declared as a known Scene id, given a fallback leaf, seeded into the default tree, and listed as structurally required (there is no layout migration — a saved layout without it is discarded for the default factory, which is fine per NO-LEGACY).

```ts
export const SCENE_PANEL_IDS = [
  "inspector", "environment", "render", "postProcess", // ← new
  "stats", "profiler", "physics", "scriptLogs", "material",
  "timeline", "hierarchy", "assets", "viewport",
] as const;
```

- `DEFAULT_LEAF` gains `postProcess: "leaf:leftBottom"` — it opens beside Inspector/Environment/Render.
- `defaultSceneLayout()` `"leaf:leftBottom"` tabs become `["inspector", "environment", "render", "postProcess"]` (leave `activeTab: "inspector"`).
- `REQUIRED_PANELS.scene` gains `"postProcess"` (it is non-closable, so `hasRequiredPanels` must count it).

> Decision to record when building: **non-closable, docked in `leaf:leftBottom` beside Render — not a closable `group:"editing"` tool.** Bloom+grade are project render config that persist in `renderSettings` (like Render and Environment), so the panel is always present and lives with its siblings, never hidden behind the Tools menu. It is a large panel and `leaf:leftBottom` is a narrow column, so the user will typically drag it out to a wider leaf — that is a runtime re-dock, not a reason to make the default home wide (the default is a starting point; `DockLayout → DockLayout` handles the move). Because it is non-closable it is *not* in `SCENE_PANEL_MENU` (Topbar filters `closable`), so no Topbar change is needed.

## The Post panel — `PostProcessPanel.tsx`

**File `editor/src/panels/PostProcessPanel.tsx` (new).**

Clone the RenderPanel skeleton verbatim: a shallow `renderStats` slice so the body re-renders only when a bloom/grade field changes (not on the 20 Hz stats poll), an `optimistic(patch)` fold of the echoed result, and a `recordRender(label, undo, redo)` that pushes into the scene tab's history. The panel reads live values from the Phase 1/3 read-back blocks on `RenderStatsDto` (`s.renderStats.bloom`, `s.renderStats.colorGrading`), never the set-command echo.

```tsx
const cfg = useEditorStore(
  useShallow((s) => {
    const b = s.renderStats?.bloom;
    const g = s.renderStats?.colorGrading;
    return {
      bloomEnabled: b?.enabled ?? false,
      bloomIntensity: b?.intensity ?? 0.05,
      bloomScatter: b?.scatter ?? 0.7,
      wbTemp: g?.whiteBalance.temp ?? 6500,
      contrast: g?.contrast ?? 1,
      saturation: g?.saturation ?? 1,
      global: g?.global ?? NEUTRAL_CDL,      // {slope, offset, power}
      shadows: g?.shadows, midtones: g?.midtones, highlights: g?.highlights,
      shadowsMax: g?.shadowsMax ?? 0.09,
      highlightsMin: g?.highlightsMin ?? 0.5,
    };
  }),
);
```

- **Writes reuse the Phase 1/3 wrappers.** Bloom scalars/toggle go through `client.setBloom(patch)`; grade fields through `client.setColorGrading(patch)` — both partial-merge calls that return the merged block, exactly the shape `client.setEnvironment(patch)` uses (`editor/src/control/client.ts`). High-frequency scrubs (wheel drags, curve drags, intensity `NumberDrag`) funnel through a per-field `makeCoalescer` (`editor/src/control/coalesce.ts`) so one `set-*` merge lands per edit-burst, honoring the serialized-control-calls rule — the `coalescerFor(field)` memo copied from `EnvironmentPanel.tsx`.
- **Undo is per-gesture.** Capture the prior at `onDragStart` (set `dragActive(true)` to gate the poll), record one `pushEdit({ label, undo, redo }, "scene")` at `onDragEnd` — the `exposurePrior`-ref pattern from RenderPanel, generalized so a `GradingWheel`/`ToneCurve` gesture records a single entry, not one per emitted sample.
- **Sectioning.** Top-level split **Bloom | Grade** via the Tabs primitive (below); within Grade, sub-sections **Global · Shadows · Midtones · Highlights** use the RenderPanel "Debug" divider pattern (`border-t border-border` + a `text-[10px] uppercase tracking-wide text-muted-foreground` `Label`). Field labels through `humanizeFieldName()`; semantic tokens only (`bg-background`/`bg-card`/`text-muted-foreground`/`border-border`), never `neutral-*`.
- **Bloom section widgets** are the existing field renderers: a `Switch` (enable), `NumberDrag` (intensity, threshold), `SliderField` (scatter/mix, 0..1), `ColorField` (tint), plus the Phase-2 dirt/anamorphic rows and the Phase-5 LUT drop slot + intensity — all lifted from the RenderPanel interim rows unchanged, just re-parented here.

## The color wheel — `GradingWheel.tsx`

**File `editor/src/components/GradingWheel.tsx` (new).**

A net-new widget: `react-colorful` (the only current color UI, in `ColorField.tsx`) is a sat/hue square with hue/alpha bars, not a trackball, so there is nothing to reuse. Draw a circular hue/sat pad (hue = pointer angle, saturation = radius, clamped to the disc) with a small draggable puck, plus a vertical **luma trim** slider beside it — the Resolve/`SColorGradingWheel` shape. The widget adopts the shared feel: `useScrubValue` for drag-local rendering (`editor/src/lib/useScrubValue.ts` — `begin`/`set`/`end`), `onDragStart`/`onDragEnd` bracketing so the panel records one undo entry and holds `dragActive`, and it emits through the panel's coalescer.

```tsx
export interface GradingWheelProps {
  /// The RGB offset around neutral the disc encodes (hue=angle, sat=radius), 0..1 linear.
  value: [number, number, number];
  /// The luma-trim scalar the vertical bar encodes.
  master: number;
  label: string;
  onChange(rgb: [number, number, number], master: number): void;
  onDragStart?(): void;
  onDragEnd?(): void;
}
```

- **Binding to the wire.** The three global wheels map to the ASC-CDL SOP the Phase 3 grade already carries (`SetColorGradingParams` `global.{slope,offset,power}`) under their Lift/Gamma/Gain aliases (Gain≈Slope, Lift≈Offset, Gamma≈Power — the canonical mapping recorded in the plan's `data_model`): the **Lift** wheel writes `global.offset`, **Gamma** writes `global.power`, **Gain** writes `global.slope`, each as `[…]` plus the master trim into the range's uniform balance. The three range wheels (Shadows/Midtones/Highlights) each drive that range's `cdl` block (Phase 4). One `GradingWheel` instance, parameterized by which grade field it patches — no per-range widget clones.
- **No new command.** Every wheel edit is a `client.setColorGrading(patch)` partial merge; the disc→RGB math is pure editor code.

> Decision to record when building: **the wheel encodes an offset around neutral, not an absolute color.** Center = no change; the puck's displacement is the CDL offset/slope/power delta. This is the Resolve/UE trackball semantic and the only one that composes with the neutral-at-rest grade — an absolute-color wheel (like `ColorField`'s square) would fight the "identity grade is untouched pixels" invariant.

## The tone curve — `ToneCurve.tsx`

**File `editor/src/components/ToneCurve.tsx` (new).**

A net-new SVG widget (no spline editor exists — `components/timeline/` is animation-specific, not reusable). Render the [0,1]×[0,1] curve area with draggable control points (add on click, drag to move, remove on right-click), a monotone/Catmull-Rom interpolation through them, and a channel selector for **master + R/G/B**. Same drag contract as the wheel (`useScrubValue`, `onDragStart`/`onDragEnd`, coalesced emit).

```tsx
export interface ToneCurveProps {
  /// Control points per channel, x/y in 0..1 (display code values).
  channels: { master: Point[]; r: Point[]; g: Point[]; b: Point[] };
  onChange(channels: ToneCurveProps["channels"]): void;
  onDragStart?(): void;
  onDragEnd?(): void;
}
```

> Decision to record when building: **the tone curve bakes into the Phase-5 creative-LUT slot, it does not add a live grade param — that is why `just engine` is unchanged.** A per-channel display-space tone curve *is* a creative LUT (the Unity color-curves-compile-into-the-grading-LUT pattern): the panel samples the curve to a small 3D `.cube`, imports it into the catalog, and assigns it to the existing `colorGrading.creativeLut` slot (Phase 5's importer + apply path). The curve edits therefore ride the exact seam Phase 5 already built — post-tonemap, `[0,1]` display-referred, with the intensity dial. A **native ALU per-channel `curves` grade op** (instant, no bake latency, evaluated in `tonemap_ops.slang`) is the more direct modern form and the cleaner long-term home; it is a small engine follow-up left as a seam (see Out of scope), deliberately not built here so this phase stays editor-only.

## Remove the migrated rows — `RenderPanel.tsx`

**File `editor/src/panels/RenderPanel.tsx`.**

The cutover. Delete every bloom/grade row added in phases 1–5 and everything that only served them: the bloom `Switch`/`NumberDrag`/`SliderField`/`ColorField` rows and dirt/anamorphic rows; the grade temp/tint/contrast/saturation `NumberDrag` rows and the interim `VectorEditor` CDL rows; the Phase-5 LUT drop slot; their `on…`/`record…` handlers; and any import (`ColorField`, `VectorEditor`, `SliderField`, `client.setBloom`, `client.setColorGrading`) now unused here. After this, `RenderPanel`'s `cfg` slice and JSX are back to the pre-Phase-1 set (AA / quality / resolution / tonemap / target-FPS / feature toggles / exposure / debug overlays) — the tonemap `Select` stays (its "View transform" relabel from Phase 3 is kept; it is display-transform config, not grade).

- Grep-verify zero `setBloom`/`setColorGrading`/`bloom`/`colorGrading` references remain in `RenderPanel.tsx`; every one lives in `PostProcessPanel.tsx`.

## Optional — a shadcn `Tabs` primitive

**File `editor/src/components/ui/tabs.tsx` (optional, new).**

The Post panel wants a top-level **Bloom | Grade** split; the sub-sections can stay Separator+label, but the top split reads better as tabs than one long scroll. `components/ui/` has no tabs/accordion primitive today.

> Decision to record when building: **add a Radix `Tabs` primitive for the top-level Bloom | Grade split; keep Separator+uppercase-label for the Grade sub-sections.** Radix `Tabs` is `role=tablist` content, not a modal overlay — it does not mount `react-remove-scroll` and does not write `--removed-body-scroll-bar-size`, so it is exempt from the scroll-lock stall the `select.tsx`/dialog rules guard against; it is safe to add as a thin shadcn wrapper. If build-time reveals the tabs are more chrome than the panel needs, the Separator+label pattern alone is the fallback with no other change — either way there is exactly one sectioning mechanism, not both stacked.

## Out of scope (later phases / clean seams)

- **Pre-encode scopes (histogram / waveform / vectorscope).** The where-the-pros-live nice-to-have listed in the plan's `ui_design`. They need a pre-encode GPU readback buffer + a `render-stats`-style read command — engine work, not editor-only — so they are a follow-up, not this phase. The Post panel leaves room for a "Scopes" section that a later phase fills.
- **A native ALU `curves` grade op.** The direct, instant per-channel curve evaluated in `tonemap_ops.slang` (a `curves` block on `SetColorGradingParams`, a grade-UBO addition). It replaces the ToneCurve→bake→creative-LUT indirection with live ALU when built. Left as a clean seam: `ToneCurve.tsx`'s control-point model is the same data a native op would consume, so the widget does not change — only its `onChange` target flips from the bake path to a `set-color-grading` `curves` patch. Not scheduled (would break the "engine unchanged" contract of this phase).

## Milestone gate

`just engine` is **unchanged** and must stay green (no engine/protocol/shader edit lands in this phase — if a change here seems to need one, it belongs in Phase 1–5). Run `bun run check` in `editor/` (`tsc --noEmit`; no `gen:protocol` regen is expected — the `CommandName` union and `RenderStatsDto` were finalized in the earlier phases) and `bun run lint` + `bun run format` (oxlint + oxfmt) and fix every warning this change raises. Because the effect is a running-editor UI, verify by driving it: `just run`, open the **Post** panel, and confirm (a) the Bloom section toggles/scrubs bloom and the reply folds back, (b) each `GradingWheel` grades its Lift/Gamma/Gain or its range's CDL and a single undo entry replays the whole gesture, (c) the `ToneCurve` reshapes tones through the baked creative-LUT slot, and (d) `RenderPanel` no longer shows any bloom/grade row. Extend a `tests/e2e` assertion only where a control round-trip is exercised (bloom/grade commands already have their Phase 1–5 e2e cases; add one only if a new interaction path appears) and run `just e2e`. Update the `docs/content/explanations/screen-space-and-post/` hub `_index.md` row cluster plus the **Bloom** and **Color grading** concept pages with the Post-panel UI (the `GradingWheel` trackball, the `ToneCurve` spline, the section layout) in the same change, and mark this plan's `README.md` `**Status:**` line and `plans/todo.md` Rendering bullet complete.
