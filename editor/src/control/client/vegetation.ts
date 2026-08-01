import { call } from "./call";
import type { CommandParamsMap } from "../../protocol";

/// Vegetation authoring, cooking, ecology, and runtime inspection.
export const vegetationCommands = {
  importVegetationAsset(path: string, folder?: string) {
    return call("import-vegetation-asset", folder ? { path, folder } : { path });
  },
  vegetationRenderStats() {
    return call("vegetation-render-stats");
  },
  vegetationRuntimeStatus() {
    return call("vegetation-runtime-status");
  },
  vegetationMutate(
    gesture: CommandParamsMap["vegetation-mutate"]["gesture"],
    records: CommandParamsMap["vegetation-mutate"]["records"],
  ) {
    return call("vegetation-mutate", { gesture, records });
  },
  plantGraph(plant: string) {
    return call("plant-graph", { plant, variation: 0 });
  },
  plantGraphSet(params: CommandParamsMap["plant-graph-set"]) {
    return call("plant-graph-set", params);
  },
  plantElements(plant: string, variation = 0) {
    return call("plant-elements", { plant, variation });
  },
  plantValidate(plant: string) {
    return call("plant-validate", { plant });
  },
  plantAtlas(params: CommandParamsMap["plant-atlas"]) {
    return call("plant-atlas", params);
  },
  plantHierarchy(params: CommandParamsMap["plant-hierarchy"]) {
    return call("plant-hierarchy", params);
  },
  plantSeasonPhenotype(params: CommandParamsMap["plant-season-phenotype"]) {
    return call("plant-season-phenotype", params);
  },
  plantProxies(params: CommandParamsMap["plant-proxies"]) {
    return call("plant-proxies", params);
  },
  plantPhenotypes(params: CommandParamsMap["plant-phenotypes"]) {
    return call("plant-phenotypes", params);
  },
  setHierarchyCut(params: CommandParamsMap["set-hierarchy-cut"] = {}) {
    return call("set-hierarchy-cut", params);
  },
  vegetationEcologyStatus() {
    return call("vegetation-ecology-status");
  },
  vegetationEcologyClock(params: CommandParamsMap["vegetation-ecology-clock"] = {}) {
    return call("vegetation-ecology-clock", params);
  },
  vegetationTelemetry() {
    return call("vegetation-telemetry");
  },
  vegetationBudgets(params: CommandParamsMap["vegetation-budgets"] = {}) {
    return call("vegetation-budgets", params);
  },
  vegetationAdvanceEcology(params: CommandParamsMap["vegetation-advance-ecology"]) {
    return call("vegetation-advance-ecology", params);
  },
  vegetationRuntimeInspect(plant: string) {
    return call("vegetation-runtime-inspect", { plant });
  },
  vegetationRuntimeQuery(params: CommandParamsMap["vegetation-runtime-query"]) {
    return call("vegetation-runtime-query", params);
  },
  vegetationPromote(plant: string) {
    return call("vegetation-promote", { plant });
  },
  vegetationDemote(plant: string) {
    return call("vegetation-demote", { plant });
  },
  vegetationMapLayerCommit(params: CommandParamsMap["vegetation-map-layer-commit"]) {
    return call("vegetation-map-layer-commit", params);
  },
  vegetationMapChunkCommit(params: CommandParamsMap["vegetation-map-chunk-commit"]) {
    return call("vegetation-map-chunk-commit", params);
  },
  vegetationMapChunkRead(params: CommandParamsMap["vegetation-map-chunk-read"]) {
    return call("vegetation-map-chunk-read", params);
  },
  vegetationCook(params: CommandParamsMap["vegetation-cook"]) {
    return call("vegetation-cook", params);
  },
  vegetationCookStatus(job: string) {
    return call("vegetation-cook-status", { job });
  },
  vegetationCancelCook(job: string) {
    return call("vegetation-cancel-cook", { job });
  },
  vegetationPreflightRegion(params: CommandParamsMap["vegetation-preflight-region"]) {
    return call("vegetation-preflight-region", params);
  },
  querySurfaceRay(params: CommandParamsMap["query-surface-ray"]) {
    return call("query-surface-ray", params);
  },
  vegetationTopologyDiff(params: CommandParamsMap["vegetation-topology-diff"]) {
    return call("vegetation-topology-diff", params);
  },
  vegetationNodeSchema() {
    return call("vegetation-node-schema", {});
  },
  vegetationCompileBiome(params: CommandParamsMap["vegetation-compile-biome"]) {
    return call("vegetation-compile-biome", params);
  },
  vegetationCancelEvaluation(job: string) {
    return call("vegetation-cancel-evaluation", { job });
  },
  vegetationStartEvaluation(job: string) {
    return call("vegetation-start-evaluation", { job });
  },
  vegetationEvaluationStatus(job: string) {
    return call("vegetation-evaluation-status", { job });
  },
  vegetationAssetSummary(asset: string) {
    return call("vegetation-asset-summary", { asset });
  },
};
