+++
title = 'Scripting'
weight = 16
bookCollapseSection = true
+++

# Scripting

Anima runs gameplay code in [Luau](https://luau.org/) during Play. An entity's `Script` component contains ordered slots, each naming a `.lua` file below the project's `src/` directory and storing per-entity field overrides.

The shared `RuntimeSession` owns the play-scene duplicate, physics world, and one `ScriptHost`. Script instances can inspect input, edit components and transforms, spawn entities, query physics, exchange messages, and schedule coroutine work through the `sa` API.

## Script shape

A script returns a class table with `on_update`. `on_create` and `on_destroy` are optional.

```lua
local Spinner = {}

Spinner.properties = { speed = 1.0 }

function Spinner:on_update(dt)
  local rotation = self.entity:get_rotation()
  rotation.y += self.speed * dt
  self.entity:set_rotation(rotation)
end

return Spinner
```

Each slot receives its own `self` table and `EntityHandle`. The class table is cached by resolved path, while declared field values are copied into the instance and overlaid with the slot's authored overrides.

## Authoring surface

Project loading ensures `src/example.lua`, generated `library/sa.lua` definitions, and `.luarc.json` configuration exist. These files give [Lua Language Server](https://luals.github.io/) type information for `sa`, entity methods, vectors, and registered component names.

The Inspector manages ordered script slots and renders controls for declared number, Boolean, string, and `sa.Vec3` fields. Script logs and contained runtime errors return to dedicated editor panels over the control plane.

## Runtime boundary

`saffron-script` is the only crate that embeds the Luau VM through `mlua`. Scoped session access lends callbacks the active scene, component registry, input snapshot, and host bridge without storing Rust scene references in script userdata.

The VM removes filesystem, process, package, and native-loading facilities. Memory and instruction budgets bound each session and callback. Load failures skip the affected slot; update and contact callback failures pause simulation and enter the error ring; teardown and scheduled-task failures are logged without terminating the host.

## Pages

| Page | Covers | Main code |
|---|---|---|
| [Lua runtime](lua-runtime/) | VM ownership, sandbox, budgets, tracebacks, and error conversion | `engine/crates/script/src/vm.rs` |
| [Script components and the play runtime](script-components-and-runtime/) | Slot data, play lifecycle, `sa` APIs, messages, tasks, and callbacks | `engine/crates/script/src/runtime.rs` |
| [Script-declared fields](script-declared-fields/) | Property discovery, defaults, per-slot overrides, and Inspector controls | `engine/crates/script/src/schema.rs` |
