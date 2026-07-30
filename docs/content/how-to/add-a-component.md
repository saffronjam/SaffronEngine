+++
title = 'Add a component'
weight = 4
math = false
+++

# Add a component

Add a new ECS component type. A single `register_component!` line wires up serialization, the CLI, and the Inspector.

## Steps

1. Declare the value struct in `component.rs` (the `saffron-scene` crate) — a plain `struct` deriving `Clone + Default`, like the existing ones:
   ```rust
   #[derive(Clone, Copy, Debug, PartialEq)]
   pub struct Health {
       /// Current hit points.
       pub hp: f32,
   }

   impl Default for Health {
       fn default() -> Self {
           Self { hp: 100.0 }
       }
   }
   ```
2. Give it a `SceneSerialize` impl in `serde.rs` — the `to_json` / `load_json` pair that carries the component's fields on the wire and to disk:
   ```rust
   impl SceneSerialize for Health {
       fn to_json(&self) -> Value {
           json!({ "hp": self.hp })
       }

       fn load_json(&mut self, value: &Value) -> Result<()> {
           self.hp = value.get("hp").and_then(Value::as_f64).unwrap_or(100.0) as f32;
           Ok(())
       }
   }
   ```
3. Register it with one line in `register_builtin_components` (`registry.rs`), and add its name to `BUILTIN_COMPONENT_NAMES` in the same file (the `registry_is_complete` test fails if the two lists drift):
   ```rust
   register_component!(reg, Health, "Health");
   ```
   There is no per-type UI draw hook: the engine renders no UI. The Inspector is the React/CEF frontend, which builds each field from the DTO catalog over the control plane. The macro defaults the serde to the type's `SceneSerialize` impl and synthesizes the structural fn-pointers (`has` / `add_default` / `remove` / `copy_to` / `serialize` / `deserialize`) into a `ComponentTraits` row. An optional trailing `bool` is `removable`, which is `false` for always-present types like `Name`, `Transform`, and `Relationship` (it defaults to `true`).
4. Mirror the body as a wire DTO in `saffron-protocol` (`scene_dto.rs`): the struct itself, an `Option<…>` field on `Components` and a variant on `ComponentBody` at the registry position, and the name in `COMPONENT_NAMES`. `inspect` is validated against `Components`, which admits no property it has no field for, and `the_component_dto_aggregate_matches_the_scene_registry` fails when the two name lists drift.
5. Regenerate the protocol artifacts with `cargo run -p xtask -- gen-protocol`, then rebuild with `cargo build --workspace`.

The stable name is the JSON key, the Inspector header, and the CLI token. Keep it consistent across all three.

## Verify

- The type shows up: `sa list-components`.
- Set it on an entity and read it back:
  ```sh
  sa add-component MyEntity Health
  sa set-component MyEntity Health --json '{"hp":42}'
  sa inspect MyEntity
  ```
- The Inspector shows it (with fields derived from the DTO catalog) under **Add Component**, and saving the scene serializes it under `"Health"`.

## In the code

| What | File | Symbols |
|---|---|---|
| `register_component!` macro | `engine/crates/scene/src/macros.rs` | `register_component!` |
| The traits row + registry | `engine/crates/scene/src/registry.rs` | `ComponentTraits`, `ComponentRegistry::register` |
| Where built-ins register | `engine/crates/scene/src/registry.rs` | `register_builtin_components`, `BUILTIN_COMPONENT_NAMES` |
| Component structs | `engine/crates/scene/src/component.rs` | the component value structs |
| The per-type serde | `engine/crates/scene/src/serde/` | `SceneSerialize` impls (`to_json` / `load_json`) |
| Generic add/get/has/remove | `engine/crates/scene/src/scene.rs` | `add_component`, `with_component`, `has_component` |
| CLI add/set/inspect | `engine/crates/control/src/commands_scene/` | `register_scene_commands` (`add-component`, `set-component`, `inspect`) |

## Related

- [Component registry](../../explanations/scene-and-ecs/component-registry/)
- [ECS architecture](../../explanations/scene-and-ecs/ecs-architecture/)
- [Scene serialization](../../explanations/scene-and-ecs/scene-serialization/)
