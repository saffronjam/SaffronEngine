import { call } from "./call";
import type { MaterialGraph } from "../../materials/graph";
import type {
  AssetList,
  AssetUsagesResult,
  CommandParamsMap,
  CommandResultMap,
  EntityRef,
  Thumbnail,
} from "../../protocol";

/// The asset catalog: browsing, folders, import/extraction, materials, and placement.
export const assetCommands = {
  listAssets(): Promise<AssetList> {
    return call("list-assets");
  },

  getThumbnail(id: string, size?: number): Promise<Thumbnail> {
    return call("get-thumbnail", size === undefined ? { asset: id } : { asset: id, size });
  },
  /// 512-px preview for the View modal (same readback path as get-thumbnail).
  viewAsset(id: string, size?: number): Promise<Thumbnail> {
    return call("view-asset", size === undefined ? { asset: id } : { asset: id, size });
  },
  /// Rename a catalog entry. Returns the new {id, name}.
  renameAsset(id: string, newName: string): Promise<{ id: string; name: string }> {
    return call("rename-asset", { asset: id, name: newName });
  },
  createAssetFolder(folder: string): Promise<AssetList> {
    return call("create-asset-folder", { folder });
  },
  renameAssetFolder(folder: string, name: string): Promise<AssetList> {
    return call("rename-asset-folder", { folder, name });
  },
  deleteAssetFolder(folder: string): Promise<AssetList> {
    return call("delete-asset-folder", { folder });
  },
  moveAsset(
    id: string,
    folder: string | null,
  ): Promise<{ id: string; name: string; folder?: string }> {
    const params: CommandParamsMap["move-asset"] = { asset: id };
    if (folder) {
      params.folder = folder;
    }
    return call("move-asset", params);
  },
  assetUsages(id: string): Promise<AssetUsagesResult> {
    return call("asset-usages", { asset: id });
  },
  /// On-disk metadata for one asset: size, vertex/triangle counts (meshes), and the
  /// file's modified time. Backs the assets-panel detail view.
  probeAsset(id: string): Promise<CommandResultMap["probe-asset"]> {
    return call("probe-asset", { asset: id });
  },
  deleteAsset(id: string): Promise<CommandResultMap["delete-asset"]> {
    return call("delete-asset", { asset: id });
  },
  /// Assign a mesh or albedo texture to an entity slot (adds the component if
  /// missing). The dedicated, minimal write for Mesh.mesh / Material.albedoTexture.
  assignAsset(
    entity: string,
    slot: "mesh" | "albedo" | "metallic-roughness",
    asset: string,
  ): Promise<unknown> {
    return call("assign-asset", { entity, slot, asset });
  },
  /// Create a new default material asset; returns its id + name.
  materialCreate(name: string) {
    return call("material-create", { name });
  },
  /// List the project's material assets (for the material browser/picker).
  materialList() {
    return call("material-list");
  },
  /// Read a material asset's fields (blend/unlit/factors/texture ids).
  materialGet(material: string) {
    return call("material-get", { material });
  },
  /// Edit a material asset's scalar factors in place.
  materialUpdate(
    material: string,
    patch: Omit<CommandParamsMap["material-update"], "material">,
  ): Promise<unknown> {
    return call("material-update", { material, ...patch });
  },
  /// Point every `MaterialSet` slot of an entity at a `.smat` material asset ("0" clears to
  /// the built-in default).
  materialAssign(entity: string, material: string): Promise<unknown> {
    return call("material-assign", { entity, material });
  },
  /// Import a folder of PBR textures, suffix-detecting roles, into a new .smat.
  materialImport(path: string, name?: string) {
    return call("material-import", { path, name: name ?? "" });
  },
  /// Replace a material's node graph; returns whether it folded entirely to params (no codegen node).
  materialSetGraph(material: string, graph: MaterialGraph) {
    return call("material-set-graph", { material, graph });
  },
  /// Compile a material's node graph to a shader via codegen; returns { id, ok }.
  materialCompileGraph(material: string) {
    return call("material-compile-graph", { material });
  },
  /// Import a model from a filesystem path: bakes one .smodel asset + catalog rows and returns the
  /// model asset ref. instantiateModel places it into the scene.
  importModel(path: string): Promise<CommandResultMap["import-model"]> {
    return call("import-model", { path });
  },
  /// Import a texture from a filesystem path into the catalog.
  importTexture(path: string): Promise<{ texture: string }> {
    return call("import-texture", { path });
  },
  /// Import a creative `.cube` look as a LUT asset into the catalog; returns the LUT asset ref that
  /// the `set-color-grading` `creativeLutAsset` slot references.
  importLut(path: string): Promise<CommandResultMap["import-lut"]> {
    return call("import-lut", { path });
  },
  /// Expand a model asset's stored hierarchy into the scene; returns the new root entity.
  instantiateModel(asset: string, name?: string): Promise<EntityRef> {
    return name === undefined
      ? call("instantiate-model", { asset })
      : call("instantiate-model", { asset, name });
  },
  /// Preview, commit, or clear a model asset placement driven by viewport drag/drop.
  previewAssetPlacement(
    asset: string,
    u: number,
    v: number,
  ): Promise<CommandResultMap["asset-placement"]> {
    return call("asset-placement", { phase: "preview", asset, u, v });
  },
  commitAssetPlacement(): Promise<CommandResultMap["asset-placement"]> {
    return call("asset-placement", { phase: "commit" });
  },
  clearAssetPlacement(): Promise<CommandResultMap["asset-placement"]> {
    return call("asset-placement", { phase: "clear" });
  },
  /// Rescan assets/ and reconcile the catalog from disk; returns added/removed counts.
  scanAssets(): Promise<CommandResultMap["scan-assets"]> {
    return call("scan-assets");
  },
  /// Slice an embedded sub-asset to a standalone file (keeping its id) + remap the container.
  extractSubAsset(
    asset: string,
    subAsset: string,
    dest?: string,
  ): Promise<CommandResultMap["extract-subasset"]> {
    return dest === undefined
      ? call("extract-subasset", { asset, subAsset })
      : call("extract-subasset", { asset, subAsset, dest });
  },
  /// Revert an extracted sub-asset to the embedded chunk.
  clearExtraction(asset: string, subAsset: string): Promise<CommandResultMap["clear-extraction"]> {
    return call("clear-extraction", { asset, subAsset });
  },
  /// Re-bake a model from its source (skip if unchanged), preserving extractions.
  reimportModel(asset: string): Promise<CommandResultMap["reimport-model"]> {
    return call("reimport-model", { asset });
  },
  /// A container's sub-assets, source recipe, and byte footprint.
  modelInfo(asset: string): Promise<CommandResultMap["model-info"]> {
    return call("model-info", { asset });
  },
  /// What references this / what this references + footprint.
  assetReferences(asset: string): Promise<CommandResultMap["asset-references"]> {
    return call("asset-references", { asset });
  },
  /// A categorized cleanup report (dry-run; never deletes).
  cleanAssets(exclude?: string[]): Promise<CommandResultMap["clean-assets"]> {
    return exclude === undefined ? call("clean-assets", {}) : call("clean-assets", { exclude });
  },
  /// Delete confirmed-unused assets (requires confirm), then rescan.
  deleteUnused(ids: string[], confirm: boolean): Promise<CommandResultMap["delete-unused"]> {
    return call("delete-unused", { ids, confirm });
  },
};
