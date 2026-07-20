+++
title = 'Script-declared fields'
weight = 3
+++

# Script-declared fields

A script declares editable instance data in its class table's `properties` field. The script owns
the defaults, while each `ScriptSlot` stores only values that differ for that entity. Changing a
default therefore affects every slot that has no override for that field.

```lua
---@class Turret : sa.ScriptSelf
---@field speed number
---@field target string
---@field enabled boolean
---@field offset sa.Vec3
local Turret = {}

Turret.properties = {
  speed = 5.0,
  target = "Player",
  enabled = true,
  offset = sa.vec3(0, 1, 0),
}

function Turret:on_update(dt)
  if self.enabled then
    local position = self.entity:get_position()
    self.entity:set_position(position + self.offset * self.speed * dt)
  end
end

return Turret
```

The annotations are optional at runtime. They let [Lua Language Server](https://luals.github.io/)
type the fields on `self` alongside the [generated editor types](../script-components-and-runtime/#generated-editor-types).

## Schema discovery

`read_script_schema` loads the file in a fresh sandboxed [Luau](https://luau.org/) VM with value types
and no-scene `sa` functions registered. The module body executes to produce its class table, but the
reader does not call `on_create` or `on_update`. Field declarations should therefore build data
without relying on scene access or gameplay callbacks.

The Inspector supports defaults with these Luau types:

| Default | Schema type | Inspector control | Wire value |
|---|---|---|---|
| number | `number` | number drag | JSON number |
| boolean | `bool` | switch | JSON boolean |
| string | `string` | text input | JSON string |
| `sa.Vec3` | `vec3` | three-axis vector editor | three-number JSON array |

String-keyed fields are returned in name order. A table, function, `nil`, or other unsupported default
is logged and omitted from the edit-time schema. A missing `properties` table produces an empty field
list, while syntax errors, runtime errors in the module body, and non-table returns fail the schema
request.

## Defaults and overrides

At play start, `build_instance` creates a `self` table for each script slot. `inject_fields` visits
every string-keyed entry in the class's `properties` table and writes either the matching slot override
or the declared default. The resulting script reads `self.speed` and `self.offset` directly.

Overrides are authored JSON stored with the scene:

```json
{
  "scriptPath": "turret.lua",
  "overrides": {
    "speed": 8.0,
    "offset": [0.0, 2.0, 0.0]
  }
}
```

An `sa.Vec3` default becomes a fresh userdata value for each instance. A table default is shallow-copied
before injection, although table defaults do not appear as editable Inspector fields. Scalar defaults
are copied directly.

Only names still present in `properties` are visited, so an override left behind after a rename has no
effect. Removing an override restores the script default on the next play session. The override remains
authored data during Play; it does not modify a running instance that already received its fields.

## Editor and control flow

The Script Inspector requests `get-script-schema` once for each distinct assigned path while the panel
is mounted. It renders a switch, text input, number drag, or vector editor according to the returned
type. Schema failures appear inline on the affected slot.

Editing a control sends `set-script-override` with the entity, slot index, field name, and JSON value.
Number and vector drags are coalesced, and one undo entry records each completed gesture. The reset
button sends `null`, which removes that name from the slot's override object.

`get-script-schema` resolves its path below the project's `src/` directory and returns
`ScriptFieldDto` values. `set-script-override` validates the entity and slot index, then stores the JSON
value without re-reading the schema. The Inspector is responsible for sending the wire shape associated
with each field type.

## In the code

| What | File | Symbols |
|---|---|---|
| Edit-time schema reader | `script/src/schema.rs` | `read_script_schema`, `ScriptField`, `ScriptFieldType`, `infer_field` |
| Instance field injection | `script/src/runtime.rs` | `build_instance`, `inject_fields`, `shallow_copy` |
| Slot override storage | `scene/src/component.rs` | `Script`, `ScriptSlot` |
| Schema control handler | `host/src/layer.rs` | `register_script_schema_command` |
| Override control handler | `control/src/commands_scene.rs` | `register_scene_commands` |
| Protocol data | `protocol/src/dto.rs` | `GetScriptSchemaParams`, `ScriptFieldDto`, `GetScriptSchemaResult`, `SetScriptOverrideParams`, `SetScriptOverrideResult` |
| Inspector widgets and reset | `editor/src/components/ScriptSlots.tsx` | `ScriptSlots`, `renderFieldWidget`, `onOverride` |

## Related

- [Script components and the play runtime](../script-components-and-runtime/) - explains slot execution and instance creation
- [Lua runtime](../lua-runtime/) - describes the VM sandbox and instruction budget
