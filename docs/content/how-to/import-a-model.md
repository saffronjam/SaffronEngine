+++
title = 'Import a model'
weight = 2
math = false
+++

# Import a model

Import the repository's cube glTF into an open project, instantiate it in the scene, and verify the baked `.smodel` container.

## Prerequisites

- Run the editor with a project open and stay in Edit mode.
- Run commands from the repository root with `sa` and `jq` available.
- Build the engine so `engine/assets/models/cube.gltf` exists beside the shader assets.

## Steps

1. Confirm the source model is present:

   ```sh
   test -s engine/assets/models/cube.gltf && echo 'source model OK'
   # source model OK
   ```

2. Import the glTF and capture the new model asset ID:

   ```sh
   import_reply="$(sa -o json import-model --path "$PWD/engine/assets/models/cube.gltf")"
   echo "$import_reply" | jq '{id, name, type}'
   # {"id":"...","name":"cube","type":"model"}

   model_id="$(echo "$import_reply" | jq -r .id)"
   ```

   The **Assets** panel adds one model tile. Importing updates the catalog but does not place an entity in the scene.

3. Check the baked container metadata:

   ```sh
   sa -o json model-info --asset "$model_id" \
     | jq '{name, nodeCount, materialCount, hasSkin, totalBytes}'
   # {"name":"cube","nodeCount":1,"materialCount":1,"hasSkin":false,"totalBytes":...}
   ```

4. Read the container path from the catalog and verify the file on disk:

   ```sh
   model_path="$(sa -o json list-assets \
     | jq -r --arg id "$model_id" '.assets[] | select(.id == $id) | .path')"
   project_root="$(sa -o json get-project | jq -r .root)"

   echo "$model_path"
   # models/<model-id>.smodel

   test -s "$project_root/assets/$model_path" && echo 'model container OK'
   # model container OK
   ```

5. Instantiate the model with a stable scene name:

   ```sh
   root_id="$(sa -o json instantiate-model --asset "$model_id" --name ImportedCube | jq -r .id)"
   sa -o json inspect --entity "$root_id" | jq '{name, componentOrder}'
   # {"name":"ImportedCube","componentOrder":[...]}
   ```

   The new root appears in the **Hierarchy** and becomes the editor selection.

6. Focus the viewport on the instance and save the project:

   ```sh
   sa -o json focus --entity "$root_id" | jq '{id, name}'
   # {"id":"...","name":"ImportedCube"}

   sa -o json save-project | jq '{loaded, name, path}'
   # {"loaded":true,"name":"my-project","path":".../project.json"}
   ```

## Verify

Capture the viewport and confirm the instance and PNG exist:

```sh
sa -o json list-entities \
  | jq -e '.entities | any(.name == "ImportedCube")' \
  && echo 'scene instance OK'
# true
# scene instance OK

sa -o json screenshot --target viewport --path /tmp/imported-cube.png
# {"target":"viewport","path":"/tmp/imported-cube.png","pending":false}

test -s /tmp/imported-cube.png && echo 'import screenshot OK'
# import screenshot OK
```

For the editor-only path, click **Import** in the **Assets** panel, choose a glTF, GLB, OBJ, or SMESH file, then right-click its model tile and choose **Add to scene**.

## Related

- [Import pipeline](../../explanations/geometry-and-assets/import-pipeline/) — source parsing, baking, and catalog registration
- [The .smodel container](../../explanations/geometry-and-assets/smodel-container/) — container chunks and sub-assets
- [glTF and OBJ import](../../explanations/geometry-and-assets/gltf-and-obj-import/) — format mapping
