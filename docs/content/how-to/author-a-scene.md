+++
title = 'Author a scene'
weight = 3
math = false
+++

# Author a scene

Build a small lit scene from native primitives, assign a material, and verify that it survives a project reload.

## Prerequisites

- Create a fresh project in the editor startup dialog and wait for the viewport to appear.
- Run the commands from a shell with `sa` and `jq` available.
- Stay in Edit mode for the whole procedure.

A fresh project contains a framed **Camera** and a directional light named **Sun**.

## Steps

1. Confirm that the project and starter scene are ready:

   ```sh
   sa -o json get-project | jq '{loaded, name, path}'
   # {"loaded":true,"name":"my-project","path":".../project.json"}

   sa -o json list-entities | jq -r '.entities[].name' | sort
   # Camera
   # Sun
   ```

2. Add a plane and a cube. Capture their IDs so later commands do not depend on unique names:

   ```sh
   floor_id="$(sa -o json add-entity --preset plane | jq -r .id)"
   sa -o json rename-entity --entity "$floor_id" --name Floor
   # {"id":"...","name":"Floor"}

   cube_id="$(sa -o json add-entity --preset cube | jq -r .id)"
   sa -o json rename-entity --entity "$cube_id" --name Centerpiece
   # {"id":"...","name":"Centerpiece"}
   ```

3. Scale the floor and place the cube above it:

   ```sh
   sa -o json set-transform --entity "$floor_id" --scale '{"x":8,"y":1,"z":8}'
   # {"id":"...","name":"Floor"}

   sa -o json set-transform --entity "$cube_id" --translation '{"x":0,"y":0.5,"z":0}'
   # {"id":"...","name":"Centerpiece"}
   ```

4. Create a material and assign it to the cube:

   ```sh
   material_id="$(sa -o json material-create --name CenterpieceMat | jq -r .id)"
   sa -o json material-update --material "$material_id" \
     --baseColor '{"x":0.12,"y":0.42,"z":0.8,"w":1}' \
     --metallic 0.1 --roughness 0.35
   # {"id":"..."}

   sa -o json material-assign --entity "$cube_id" --material "$material_id"
   # {"material":"..."}
   ```

5. Aim the starter sun and raise its intensity:

   ```sh
   sa -o json set-light --entity Sun \
     --direction '{"x":-0.5,"y":-1,"z":-0.3}' --intensity 3
   # {"id":"...","name":"Sun"}
   ```

6. Save the active project:

   ```sh
   sa -o json save-project | jq '{loaded, name, path}'
   # {"loaded":true,"name":"my-project","path":".../project.json"}
   ```

7. Reload the saved project and wait for its non-blocking load to finish:

   ```sh
   sa -o json reload-project | jq -r .phase
   # loading

   until [ "$(sa -o json project-status | jq -r .phase)" = ready ]; do
     sleep 0.1
   done

   sa -o json list-entities | jq -r '.entities[].name' | sort
   # Camera
   # Centerpiece
   # Floor
   # Sun
   ```

## Verify

Capture the viewport and confirm that the file is non-empty:

```sh
sa -o json screenshot --target viewport --path /tmp/saffron-scene.png
# {"target":"viewport","path":"/tmp/saffron-scene.png","pending":false}

test -s /tmp/saffron-scene.png && echo 'scene screenshot OK'
# scene screenshot OK
```

The viewport should show a blue cube on a large plane. `sa -o json inspect --entity Centerpiece` should include `Transform`, `Mesh`, and `MaterialSet` components.

## Related

- [ECS architecture](../../explanations/scene-and-ecs/ecs-architecture/) — entities, components, and scene access
- [Built-in components](../../explanations/scene-and-ecs/built-in-components/) — component fields and defaults
- [Project serialization](../../explanations/geometry-and-assets/project-serialization/) — project save and load structure
- [Native materials](../../explanations/materials-and-pipelines/native-materials/) — material assets and entity slots
