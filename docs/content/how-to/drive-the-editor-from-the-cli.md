+++
title = 'Drive the editor from the CLI'
weight = 5
math = false
+++

# Drive the editor from the CLI

Create and edit an entity in a running editor through the `sa` control client, then capture the resulting viewport.

## Prerequisites

- Complete [Build and run](../build-and-run/).
- Install `jq`.
- Open a project in the editor and wait for **Preparing renderer...** to disappear.

Keep the editor running in one terminal. Run the commands below from a second terminal at the repository root.

## Steps

1. Put the built CLI on this shell's path and check the control socket:

   ```sh
   export PATH="$PWD/engine/target/debug:$PATH"
   sa ping
   # pong  engine=SaffronAnima  version=<version>  pid=<pid>
   ```

2. Ask the live registry for its command list:

   ```sh
   sa help | sed -n '1,6p'
   #   ping                    liveness + engine info
   #   help                    list available commands
   ```

3. Create a native cube and capture its wire ID:

   ```sh
   cube_id="$(sa -o json add-entity --preset cube | jq -r .id)"
   echo "$cube_id"
   # 1844674407370955161
   ```

   The displayed decimal ID varies by project. The new cube appears in the **Hierarchy** and becomes the editor selection.

4. Rename and position the cube with named DTO fields:

   ```sh
   sa -o json rename-entity --entity "$cube_id" --name CliCube
   # {"id":"1844674407370955161","name":"CliCube"}

   sa -o json set-transform --entity "$cube_id" \
     --translation '{"x":0,"y":1,"z":0}' \
     --scale '{"x":1.5,"y":1.5,"z":1.5}'
   # {"id":"1844674407370955161","name":"CliCube"}
   ```

5. Select the cube, focus the editor camera, and read its transform back:

   ```sh
   sa -o json select --entity "$cube_id"
   # {"id":"1844674407370955161","name":"CliCube"}

   sa -o json focus --entity "$cube_id"
   # {"id":"1844674407370955161","name":"CliCube"}

   sa -o json inspect --entity "$cube_id" | jq '.components.Transform'
   # {"translation":{"x":0.0,"y":1.0,"z":0.0},...}
   ```

   The **Inspector** updates to `CliCube`, and the viewport camera aims at it.

6. Capture the offscreen viewport image:

   ```sh
   sa -o json screenshot --target viewport --path /tmp/sa-cli-view.png
   # {"target":"viewport","path":"/tmp/sa-cli-view.png","pending":false}
   ```

## Verify

Confirm the engine selection and the PNG file:

```sh
sa -o json get-selection | jq '{entity, selectionVersion, sceneVersion}'
# {"entity":{"id":"1844674407370955161","name":"CliCube"},...}

test -s /tmp/sa-cli-view.png && echo 'CLI viewport capture OK'
# CLI viewport capture OK
```

Use `sa -o json <command>` for scripts so replies remain structured. Named flags use the command DTO's camelCase field names; see the [control command reference](../../reference/control-commands/) for the complete inventory.

## Related

- [sa CLI](../../explanations/tooling-and-control/sa-cli-protocol/) — argument coercion, output modes, and exit codes
- [Control plane](../../explanations/tooling-and-control/control-plane-architecture/) — socket dispatch and editor reconciliation
- [Picking](../../explanations/scene-and-ecs/picking/) — the viewport selection path
