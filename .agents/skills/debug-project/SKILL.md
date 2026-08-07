---
name: debug-project
description: Debug an Anima user project or saved scene by loading the project from the default appdata/userdata location, driving the running host over the control plane, taking viewport screenshots, and collecting render-stats/profiler captures. Use when investigating visual bugs, scene-load issues, renderer regressions, object/light movement artifacts, or loaded-project performance.
---

# Debug Project

Use this skill when a task depends on a saved Anima project rather than a synthetic test scene.
Treat the saved project as a repro fixture, not as proof of the issue: inspect the code path and
verify behaviour with screenshots, render stats, traces, or targeted tests.

## Project Location

User projects live under `./appdata/userdata/` by default. A project named `Test` is normally:

```sh
appdata/userdata/test/project.json
```

If the user gives a different project path or name, use that instead. For ad-hoc engine boots, set:

```sh
export SAFFRON_APPDATA_DIR="$PWD/appdata"
export SAFFRON_PROJECT="$PWD/appdata/userdata/test/project.json"
```

Keep these variables inside the `saffron-build` toolbox invocation when the host is launched from
a toolbox command.

## Boot a Saved Project

Use the project-standard GPU setup. Do not hand-roll Vulkan ICD paths.

```sh
toolbox run -c saffron-build bash -lc '
  cd /var/home/saffronjam/repos/saffron-anima
  source tools/gpu-driver.sh
  export SAFFRON_EDITOR_NATIVE_VIEWPORT=1
  export SAFFRON_APPDATA_DIR="$PWD/appdata"
  export SAFFRON_PROJECT="$PWD/appdata/userdata/test/project.json"
  export SAFFRON_CONTROL_SOCK="/tmp/anima-debug-$$.sock"
  engine/target/debug/saffron-host
'
```

Prefer a unique `SAFFRON_CONTROL_SOCK` for scripted runs. If an editor is already open, do not kill
its host unless the user explicitly asks; inspect `/proc/<pid>/environ` first.

## Drive the Scene

Use the `sa` CLI or a small TypeScript driver against the control socket. Common commands:

```sh
engine/target/debug/sa render-stats
engine/target/debug/sa screenshot --target viewport --path /tmp/anima-debug.png
engine/target/debug/sa set-transform --entity <entity-id> --translation '{"x":0,"y":1,"z":0}'
engine/target/debug/sa profiler.set-mode --mode timestamps
engine/target/debug/sa profiler.capture-start --mode frames --frame-count 120
engine/target/debug/sa profiler.capture-stop
```

When reproducing movement bugs, move the exact object or light the user describes and capture both
the moving state and the settled state. For flicker or transient bugs, take several screenshots.

## Screenshots

Use viewport screenshots to prove what the renderer produced. Save them under `/tmp` unless the user
asks for a repo artifact.

```sh
engine/target/debug/sa screenshot --target viewport --path /tmp/anima-repro-before.png
engine/target/debug/sa screenshot --target viewport --path /tmp/anima-repro-after.png
```

Inspect screenshots directly when possible. Compare pairs with image diffs when the bug is subtle.

## Performance Captures

For renderer, shadow, scene-upload, and editor-interaction issues, check performance with the loaded
project. For the small `Test` scene, sustained frame or pass cost above 3-4 ms is a bug unless the
task proves otherwise.

Recommended loop:

1. Build first: `just engine`.
2. Load the saved project on the real GPU.
3. Enable timestamp profiling: `profiler.set-mode --mode timestamps`.
4. Start a frame capture.
5. Perform the movement or interaction from the report.
6. Stop capture and summarize the trace by pass name.
7. Record `render-stats` samples during or after the interaction.

Useful fields:

- `execute-render-graph`: total graph execution cost.
- `vsm-pages`: virtual shadow page rendering cost.
- `scene-lighting`: lighting and shadow sampling cost.
- `render-stats.vsm`: page requests, dirty marks, renders, and overflows.
- `render-stats.vsm.point`: point-light VSM activity by cube-face page family.

Separate control-plane or screenshot overhead from renderer cost. A command-driven repro can spike
`poll-control` or `on-update`; use no-screenshot movement captures to measure pure render cost.

## Verification

After a fix, run the narrow test first, then the project gate:

```sh
toolbox run -c saffron-build bash -lc 'cd /var/home/saffronjam/repos/saffron-anima/engine && cargo test -p saffron-rendering vsm::tests -- --nocapture'
toolbox run -c saffron-build bash -lc 'cd /var/home/saffronjam/repos/saffron-anima && just engine'
toolbox run -c saffron-build bash -lc 'cd /var/home/saffronjam/repos/saffron-anima && just prepare-for-commit'
```

If docs change, run the docs build, link check, and style check for the edited page. Report exact
screenshots, profile JSON paths, important timings, and whether Vulkan validation errors appeared.
