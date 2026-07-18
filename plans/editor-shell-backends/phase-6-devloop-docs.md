# Phase 6 — justfile dev loop, docs, Linux audit

**Status:** COMPLETED — pending the user's Linux gate run + macOS interactive pass.

- `justfile` Darwin branches: drop `SAFFRON_CEF_SWITCHES="ozone-platform=x11"` (Ozone is
  Linux-only); `cef_gate` Darwin variant runs the `bundle` bin incrementally (rebuild binaries,
  rsync into the existing `.app`, gate freshness on the version-locked `CEF_PATH` dir the way
  Linux gates on `icudtl.dat`), exports `DYLD_FALLBACK_LIBRARY_PATH` per the cef-rs README, and
  execs the bundle's **inner binary** with `SAFFRON_DEV_URL` (preserves env — not `open`);
  `run-software` Darwin = `--disable-gpu` (GPU-composited OSR is the default path; software OSR
  regressed ≥ M141).
- Docs (same-change rule): update
  `docs/content/explanations/ui-and-editor/editor-shell-and-viewport-bridge.md` and
  `viewport-compositing.md` for the backend architecture (module contract, AppKit presentation
  stack); hub `_index.md` rows if pages are added.
- Linux audit: `git diff main -- editor/shell editor/src Cargo.toml justfile` — every
  Linux-reachable line is either a Phase-1 mechanical move or inside a Darwin-only branch.

## Verify

macOS: `just run` cold start → interactive editor with live viewport; full manual pass of the
frozen contract surface (every `getCurrentWindow()` method, drag-drop, dialogs,
`set_viewport_*`, engine lifecycle, `serve_trace`). Linux (user): full `just check` + one manual
`just run` session.
