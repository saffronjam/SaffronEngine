+++
title = 'How-to'
weight = 20
bookCollapseSection = true
+++

# How-to

How-to guides are task-focused recipes for a known goal: each page states the steps and their order. For the underlying "why", follow the links into [Explanations](../explanations/).

## Pages

| Page | Covers |
|---|---|
| `build-and-run` | the `saffron-build` toolbox, the Cargo workspace, running the editor |
| `import-a-model` | glTF/OBJ via drag-drop, File ▸ Import, or `sa import-model` |
| `author-a-scene` | create entities, add lights and a camera, save the project |
| `add-a-component` | one `register_component!` call → serialization + inspector |
| `author-a-material-graph` | the node-graph material editor + the `sa` material commands |
| `drive-the-editor-from-the-cli` | the `sa` CLI: select, set-transform, render-stats, screenshot |
| `write-a-slang-shader` | add a `.slang`, wire it into the `xtask shaders` compile, use the PSO |
| `toggle-render-features` | `sa set-aa` / `set-clustered` / `set-ssao` / `set-gi` / `set-depth-prepass` |
| `clean-unused-assets` | the deliberate cleanup workflow: `sa clean-assets` (dry-run) → `delete-unused --confirm` |
| `export-a-standalone-app` | cook the project into a Linux directory or macOS `.app` via **Export App…** or `sa export` |
