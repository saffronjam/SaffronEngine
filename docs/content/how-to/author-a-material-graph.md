+++
title = 'Author a material graph'
weight = 8
math = false
+++

# Author a material graph

Create a material, build a procedural base-color graph, and verify it on the live preview sphere.

## Prerequisites

- Run the editor with a project open.
- Show the **Material** panel in the right dock.
- Keep the viewport visible so the asset-preview surface can render.

## Build the graph

1. Open the **Material** panel and click **New**. The material selector changes to **Material**, and a preview sphere appears.

2. Click **Graph**. A **Material graph** main tab opens with a node canvas and a live **Preview** pane.

3. Right-click an empty part of the canvas and add these nodes from the context menu:

   - **Input > Constant**
   - **Input > Texture Slot**
   - **Math > Multiply**
   - **Output > Material Output**

4. Set the **Constant** value to `[0.6, 0.3, 0.1, 1.0]`. Leave **Texture Slot** set to `albedo`.

5. Connect the pins in this order:

   - **Constant rgba** to **Multiply a**
   - **Texture Slot rgba** to **Multiply b**
   - **Multiply rgba** to **Material Output baseColor**

   After the 500 ms apply delay, the toolbar reports `applied (codegen)`. The preview sphere updates with the graph output.

6. Click **Compile**. The toolbar reports `compiled OK`. A notification and `compile failed` status indicate a shader compiler error instead.

7. Close the **Material graph** main tab, select the same material again, and click **Graph**. The four nodes and their connections reappear.

## CLI equivalent

The same graph can be written through [`sa`](../drive-the-editor-from-the-cli/). Run these commands while the editor project is open:

```sh
material_id="$(sa -o json material-create --name Rock | jq -r .id)"
echo "$material_id"
# 1844674407370955161

graph='{"nodes":[{"id":"c","type":"constant","props":{"value":[0.6,0.3,0.1,1]}},{"id":"t","type":"textureSlot","props":{"slot":"albedo"}},{"id":"m","type":"multiply"},{"id":"out","type":"materialOutput"}],"edges":[{"from":["c","rgba"],"to":["m","a"]},{"from":["t","rgba"],"to":["m","b"]},{"from":["m","rgba"],"to":["out","baseColor"]}]}'

sa -o json material-set-graph --material "$material_id" --graph "$graph"
# {"id":"1844674407370955161","foldable":false}

sa -o json material-compile-graph --material "$material_id"
# {"id":"1844674407370955161","ok":true}
```

The displayed ID varies by project. A graph containing the **Multiply** node reports `foldable: false`; a graph made only from foldable constants or texture assets can report `true`.

## Verify

Confirm all of the following:

- the graph toolbar reports `compiled OK`;
- reopening the graph restores its nodes and edges;
- the preview sphere shows the multiplied base color;
- the CLI form reports `foldable: false` and `ok: true` when used.

## Related

- [Node-graph codegen](../../explanations/materials-and-pipelines/node-graph-codegen/) — graph folding, Slang emission, and compilation
- [Native materials](../../explanations/materials-and-pipelines/native-materials/) — the `.smat` asset that stores the graph
- [Material graph live preview](../../explanations/ui-and-editor/material-graph-live-preview/) — the preview surface and editor state flow
