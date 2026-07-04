+++
title = 'Author a scene'
weight = 3
math = false
+++

# Author a scene

Create entities, add lights and a camera, and save the project from the CLI.

Start with an active project. In the editor, use the startup modal. From a shell,
`SAFFRON_PROJECT=<project-name>` is the simplest test path.

## Steps

1. Create an entity:
   ```sh
   sa create-entity Floor
   ```
2. Give it a mesh from the catalog (`sa list-assets` lists ids and names), then place it:
   ```sh
   se assign-asset Floor mesh cube
   sa set-transform Floor --scale '{"x":10,"y":0.2,"z":10}'
   ```
   `set-transform` merges the passed fields over the current value. Rotation is Euler radians; every field is an `{x,y,z}` object.
3. Add a directional light:
   ```sh
   sa create-entity Sun
   sa add-component Sun DirectionalLight
   sa set-light Sun --direction '{"x":-0.5,"y":-1,"z":-0.3}' --intensity 3
   ```
   For dynamic lights, use `add-component <entity> PointLight` or `SpotLight`.
4. Add a camera:
   ```sh
   sa create-entity Camera
   sa add-component Camera Camera
   ```
5. Tint a surface: give it a material, then set its factors (`material-update` merges the fields you name):
   ```sh
   sa material-create --name FloorMat
   sa material-assign --entity Floor --material FloorMat
   sa material-update --material FloorMat --baseColor '{"x":0.8,"y":0.8,"z":0.8,"w":1}' --roughness 0.9
   ```
6. Save the active project (catalog + scene):
   ```sh
   sa save-project
   ```

The editor offers the same operations: the **Create** menu, the in-viewport gizmo (W/E/R cycle translate/rotate/scale), and the Inspector.

## Verify

- Confirm the tree: `sa list-entities`.
- Dump one entity: `sa inspect Floor`.
- Screenshot it: `sa screenshot viewport /tmp/scene.png`.
- Reload to confirm round-trip: `sa open-project <project-name>` or `sa load-project project.json`.

## In the code

| What | File | Symbols |
|---|---|---|
| Entities + components + transform | `engine/crates/control/src/commands_scene.rs` | `create-entity`, `add-component`, `set-transform` |
| Lights + material | `engine/crates/control/src/commands_scene.rs` | `set-light`, `set-component-field` |
| Assign catalog assets | `engine/crates/control/src/commands_asset.rs` | `assign-asset` |
| Save / load project | `engine/crates/control/src/commands_asset.rs` | `save-project`, `load-project` |

## Related

- [Built-in components](../../explanations/scene-and-ecs/built-in-components/)
- [Project serialization](../../explanations/geometry-and-assets/project-serialization/)
- [Picking](../../explanations/scene-and-ecs/picking/)
