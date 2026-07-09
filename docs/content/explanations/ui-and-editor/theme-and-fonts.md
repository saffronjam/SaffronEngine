+++
title = 'Theme & fonts'
weight = 10
+++

# Theme & fonts

A theme is the set of colors and fonts that gives every editor surface one consistent look. The editor's React UI is built with shadcn/ui (Radix primitives copied into the repo) on Tailwind CSS v4. The theme keeps the normal shadcn token shape, with neutral colors and system fonts as the default.

## The palette as shadcn tokens

shadcn styles every component from a small set of CSS custom properties (`--background`, `--primary`, `--border`, …). The editor sets those variables in `styles.css`, and the rest of the UI consumes semantic Tailwind utilities such as `bg-background`, `text-muted-foreground`, and `border-border`:

```css
:root {
  --background: oklch(1 0 0);
  --foreground: oklch(0.145 0 0);
  --primary: oklch(0.205 0 0);
  --border: oklch(0.922 0 0);
  --radius: 0.5rem;
}

.dark {
  --background: oklch(0.145 0 0);
  --foreground: oklch(0.985 0 0);
  --primary: oklch(0.922 0 0);
  --border: oklch(1 0 0 / 10%);
}
```

`index.html` pins `class="dark"` on `<html>`, so the editor starts in dark mode. An `@theme inline` block re-exports each variable as a Tailwind color (`--color-background: var(--background)`), so utilities like `bg-background` and `border-border` resolve through the theme tokens instead of hard-coded colors.

## Fonts

The theme defaults to system sans and monospace stacks. Roboto and Roboto Mono remain bundled and declared with `@font-face` so they are available without a font CDN if a component later opts into them:

```css
@font-face { font-family: "Roboto";      src: url("./assets/fonts/Roboto-Regular.ttf") format("truetype"); }
@font-face { font-family: "Roboto Mono";  src: url("./assets/fonts/RobotoMono-Regular.ttf") format("truetype"); }
```

`--font-sans` and `--font-mono` are exported through `@theme inline`, which lets Tailwind utilities use the app-level font choices consistently. Data-heavy fields can still use `font-mono` for aligned numbers and ids.

## The dock surfaces

Every editor surface paints with these tokens, and they are arranged by the
[dock system](../dock-system/): a tree of resizable splits and tab groups, built on
`react-resizable-panels` (shadcn's `resizable`), one tree per dockspace-bearing main tab. That
page is the authoritative account of the layout — the tab strip, drag-to-dock, the portal host,
the locked viewport leaf, and persistence. The theme's only stake in it is that each leaf body
paints `bg-background` so the live viewport shows through only the one transparent (locked)
leaf, never a sibling panel.

## In the code

| What | File | Symbols |
|---|---|---|
| shadcn tokens | `editor/src/styles.css` | `:root` / `.dark` vars, `@theme inline` |
| Font defaults | `editor/src/styles.css` | `@font-face`, `--font-sans` / `--font-mono` |
| Layout-settled bus | `editor/src/app/layoutBus.ts` | `emitLayoutSettled`, `onLayoutSettled` |

## Related

- [Dock system](../dock-system/) — the resizable split-and-tab layout these tokens dress
- [Editor shell and the viewport bridge](../editor-shell-and-viewport-bridge/) — the shell the theme dresses
- [Viewport panel](../viewport-panel/) — the panel that fills the dock center
- [Inspector](../inspector/) — uses the mono font for its data fields
