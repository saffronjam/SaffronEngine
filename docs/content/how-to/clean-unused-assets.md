+++
title = 'Clean unused assets'
weight = 9
math = false
+++

# Clean unused assets

Use the cleanup report to identify catalog assets that the active scene cannot reach, then delete only
the IDs you have reviewed.

## Prerequisites

- A running Anima host and a working `sa` connection.
- A loaded project with the scene you want to preserve active.
- A backup or version-control checkpoint for the project directory.
- `jq` for the report filters below.

> [!WARNING]
> `delete-unused` removes asset files and matching `.smeta` sidecars. Restore requires a project backup
> or version-control history.

## Steps

1. Make the authored scene the active view:

   ```sh
   sa -o json set-active-view scene
   ```

   Expected output is `{ "view": "scene" }`. This also closes an active asset preview. Cleanup roots
   then come from the project scene.

2. Write the read-only cleanup report to a temporary file:

   ```sh
   sa -o json clean-assets > /tmp/saffron-clean-report.json
   ```

   Expected output has a `candidates` array and `reclaimableBytes` total:

   ```json
   {
     "candidates": [
       {
         "id": "7349201",
         "path": "textures/7349201.png",
         "category": "unused",
         "bytes": 24576,
         "reason": "not reachable from the active scene"
       }
     ],
     "reclaimableBytes": 24576
   }
   ```

3. List the candidates that are eligible for deletion:

   ```sh
   jq -r '.candidates[] | select(.category == "unused") | [.id, .bytes, .path] | @tsv' \
     /tmp/saffron-clean-report.json
   ```

   Review every path. The command accepts only IDs that are still classified as `unused` when deletion
   begins.

4. Inspect candidates that require repair or manual review:

   ```sh
   jq -r '.candidates[] | select(.category != "unused") | [.category, .id, .reason] | @tsv' \
     /tmp/saffron-clean-report.json
   ```

   A `review` candidate is referenced only by a numeric asset ID in a script override. A `broken`
   candidate is referenced by the scene or an asset but has no catalog entry. Neither category is
   deleted by `delete-unused`.

5. Keep any unreferenced asset that belongs outside the active scene by adding it as an exclusion root,
   then review the new report:

   ```sh
   sa -o json clean-assets --exclude '["7349201","7349202"]' \
     > /tmp/saffron-clean-report.json
   ```

   Excluding an asset also keeps assets reachable from it. Use string IDs inside the JSON array.

6. Delete the confirmed IDs with an explicit JSON array and confirmation flag:

   ```sh
   sa -o json delete-unused --ids '["7349203","7349204"]' --confirm
   ```

   Expected output reports the number of files accepted for deletion and their byte total:

   ```json
   {
     "deleted": 2,
     "reclaimedBytes": 49152
   }
   ```

   Compare `deleted` with the number of submitted IDs. An ID that gained a reference or was not
   classified as `unused` is refused and does not increase the count.

## Verify

1. Run the report again:

   ```sh
   sa -o json clean-assets > /tmp/saffron-clean-after.json
   ```

2. Confirm that the deleted IDs are absent:

   ```sh
   jq -e '[.candidates[].id] | index("7349203") == null' /tmp/saffron-clean-after.json
   ```

   `jq` exits with status `0` when the ID is absent.

3. Confirm that a known retained asset remains in the catalog:

   ```sh
   sa -o json list-assets | jq -e '.assets[] | select(.id == "KEEP_ASSET_ID")'
   ```

4. Open the active scene in the editor and verify that its models, materials, and textures still resolve.

## How classification works

`clean-assets` starts from asset references in the active scene and optional `exclude` roots, then walks
model-container and material-texture dependencies. Keeping a model container or one of its embedded
children keeps the complete container unit. Embedded sub-assets are not separate deletion candidates.

Numeric string IDs found recursively in script-slot overrides are treated as indirect references and
reported as `review`. `reclaimableBytes` includes only `unused` candidates. `delete-unused` rebuilds the
report, waits for the GPU, clears asset caches, deletes eligible files, rescans the asset directory, and
writes the refreshed catalog cache.

## In the code

| What | File | Symbols |
|---|---|---|
| Dependency reachability | `assets/src/manage.rs` | `build_dependency_graph`, `DependencyGraph` |
| Candidate classification | `assets/src/manage.rs` | `CleanCategory`, `analyze_clean`, `collect_script_referenced_ids` |
| Confirmed deletion | `assets/src/manage.rs` | `delete_unused`, `DeleteUnusedData` |
| Control handlers | `control/src/commands_asset.rs` | `register_asset_commands` |
| Command data | `protocol/src/dto.rs` | `CleanAssetsParams`, `CleanReport`, `DeleteUnusedParams`, `DeleteUnusedResult` |

## Related

- [The .smodel container](../../explanations/geometry-and-assets/smodel-container/)
- [Asset server and catalog](../../explanations/geometry-and-assets/asset-server-and-catalog/)
- [Control commands](../../reference/control-commands/)
