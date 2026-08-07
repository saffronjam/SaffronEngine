# Phase 3 — Project management: derived slug, hide, delete

**Status:** COMPLETED

Project handling in the launcher has the simple shape: creating a project asks for **one display
name** and derives the on-disk slug (`"My Project"` → `my-project`) with a live
`userdata/<slug>` preview and a collision probe; each recent row carries a kebab menu with
**Hide** (drop the row, disk untouched) and **Delete** (full on-disk removal behind a
confirmation, fenced to the userdata root). Recents are a **pure MRU** — the list is exactly the
projects recently opened, with no filesystem discovery.

## Shape (as built)

- **Recents are the registry.** `recent-projects.json` in editor appdata stays the only project
  list; nothing scans `userdata/` for unopened projects. Hide *is* row removal — there is no
  hidden flag and no collapsed section.
- **One name input.** `deriveProjectSlug` (`editor/src/launcher/projectName.ts`) lowercases,
  hyphenates whitespace/underscores, strips everything outside `[a-z0-9-]`, collapses and trims
  hyphens, and clamps to 63 chars; every non-empty result passes `validProjectName` (the mirror of
  the engine's `valid_project_name`). The create form (`PickerCard.tsx` `CreateForm`) previews the
  target path live and probes `project_name_available` (debounced) for collisions; the engine
  remains the naming authority at session boot.
- **Delete is fenced.** `delete_project` (`editor/shell/src/commands.rs`,
  `resolve_project_delete_target`) canonicalizes the selection (a project dir or its
  `project.json`), requires it to sit strictly under `userdata_dir()` and to contain
  `project.json`, then removes the tree and the recents row. The picker offers Delete only for
  rows under the userdata root; external projects get Hide only.
- **Open is one dialog.** A single `project.json`-filtered file dialog covers both the folder and
  file cases — opening a folder means navigating into it and picking its `project.json`.
- **Shell surface**: `remove_recent_project`, `delete_project`, `project_name_available` in
  `commands.rs` (unit-tested: the userdata fence, the missing-`project.json` refusal, exact-row
  removal), with `settings::remove_recent` as the shared row mutation.

## Verification (done)

- `bun test editor/src/launcher/` — name rule + slug derivation (41 tests).
- Shell `cargo clippy -- -D warnings` + `cargo test` — fence and recents tests.
- Live: create via display name lands in `userdata/<slug>` with the display name; Hide leaves the
  tree on disk; Delete removes it and its row; external rows offer no Delete.
