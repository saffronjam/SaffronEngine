import { AssetPicker } from "../../../components/AssetPicker";
import { PropertySection } from "../../../components/PropertySection";
import type { Environment } from "../../../protocol";
import { NumberRow, Row, SwitchRow, VectorRow } from "../Row";
import type { SectionProps } from "../sectionContext";

type Cloud = Environment["cloud"];
type Wind = Environment["wind"];

/// Renderer-cost knobs live in the Render panel, so they are excluded from the cloud section's
/// dirty mark and its reset.
const CLOUD_COST_FIELDS = ["primarySteps", "lightSteps", "temporalFactor"];

const authoredCloudFields = (cloud: Cloud): (keyof Cloud)[] =>
  (Object.keys(cloud) as (keyof Cloud)[]).filter((field) => !CLOUD_COST_FIELDS.includes(field));

export function CloudsSection({ ctx }: SectionProps) {
  const { env, defaults, allDetails, editor, isModified } = ctx;
  const cloud = env.cloud;
  const patchCloud = <K extends keyof Cloud>(field: K, value: Cloud[K]): void =>
    editor.patchBlock("cloud", field, value);

  return (
    <PropertySection
      title="Clouds"
      summary={cloud.enabled ? `${Math.round(cloud.coverage * 100)}% coverage` : "Disabled"}
      open={ctx.sectionOpen("clouds")}
      onOpenChange={(open) => ctx.setSectionOpen("clouds", open)}
      modified={
        defaults !== null &&
        authoredCloudFields(cloud).some((field) => isModified(cloud[field], defaults.cloud[field]))
      }
      onReset={() => ctx.resetNested("Reset clouds", "cloud", authoredCloudFields(cloud))}
    >
      <SwitchRow
        label="Clouds"
        checked={cloud.enabled}
        onCheckedChange={(checked) => patchCloud("enabled", checked)}
      />

      {cloud.enabled ? (
        <>
          <NumberRow
            label="Coverage"
            editor={editor}
            value={cloud.coverage}
            min={0}
            max={1}
            step={0.005}
            onChange={(value) => patchCloud("coverage", value)}
          />
          <NumberRow
            label="Cloud Type"
            editor={editor}
            value={cloud.cloudType}
            min={0}
            max={1}
            step={0.005}
            onChange={(value) => patchCloud("cloudType", value)}
          />
          <NumberRow
            label="Precipitation"
            editor={editor}
            value={cloud.precipitation}
            min={0}
            max={1}
            step={0.005}
            onChange={(value) => patchCloud("precipitation", value)}
          />
          {allDetails ? (
            <>
              <NumberRow
                label="Anvil Bias"
                editor={editor}
                value={cloud.anvilBias}
                min={0}
                max={1}
                step={0.005}
                onChange={(value) => patchCloud("anvilBias", value)}
              />
              <NumberRow
                label="Layer Altitude"
                editor={editor}
                value={cloud.layerAltitude}
                min={-1000}
                max={20000}
                step={10}
                onChange={(value) => patchCloud("layerAltitude", value)}
              />
              <NumberRow
                label="Layer Height"
                editor={editor}
                value={cloud.layerHeight}
                min={1}
                max={20000}
                step={10}
                onChange={(value) => patchCloud("layerHeight", value)}
              />
              <NumberRow
                label="Base Scale"
                editor={editor}
                value={cloud.baseScale}
                min={0.000001}
                max={0.01}
                step={0.000001}
                onChange={(value) => patchCloud("baseScale", value)}
              />
              <NumberRow
                label="Detail Scale"
                editor={editor}
                value={cloud.detailScale}
                min={0.000001}
                max={0.1}
                step={0.00001}
                onChange={(value) => patchCloud("detailScale", value)}
              />
              <NumberRow
                label="Detail Strength"
                editor={editor}
                value={cloud.detailStrength}
                min={0}
                max={1}
                step={0.005}
                onChange={(value) => patchCloud("detailStrength", value)}
              />
              <NumberRow
                label="Curl Strength"
                editor={editor}
                value={cloud.curlStrength}
                min={0}
                max={2000}
                step={1}
                onChange={(value) => patchCloud("curlStrength", value)}
              />
              <NumberRow
                label="Weather Scale"
                editor={editor}
                value={cloud.weatherScale}
                min={0.000001}
                max={0.01}
                step={0.000001}
                onChange={(value) => patchCloud("weatherScale", value)}
              />
              <VectorRow
                label="Weather Offset"
                editor={editor}
                axes={["x", "z"]}
                labels={["X", "Z"]}
                value={cloud.weatherOffset as unknown as Record<string, number>}
                step={10}
                onChange={(channels) =>
                  patchCloud("weatherOffset", { ...cloud.weatherOffset, ...channels })
                }
              />
              <Row label="Weather Map">
                <AssetPicker
                  value={cloud.weatherTexture}
                  assetType="texture"
                  onChange={(id) => patchCloud("weatherTexture", id)}
                />
              </Row>
              <NumberRow
                label="Droplet Diameter"
                editor={editor}
                value={cloud.dropletDiameter}
                min={5}
                max={50}
                step={0.1}
                onChange={(value) => patchCloud("dropletDiameter", value)}
              />
            </>
          ) : null}
          <SwitchRow
            label="Cast Shadows"
            checked={cloud.castCloudShadows}
            onCheckedChange={(checked) => patchCloud("castCloudShadows", checked)}
          />
          {cloud.castCloudShadows && allDetails ? (
            <>
              <NumberRow
                label="Cloud Shadow"
                editor={editor}
                value={cloud.cloudShadowStrength}
                min={0}
                max={1}
                step={0.005}
                onChange={(value) => patchCloud("cloudShadowStrength", value)}
              />
              <NumberRow
                label="Surface Shadow"
                editor={editor}
                value={cloud.cloudShadowOnSurfaceStrength}
                min={0}
                max={1}
                step={0.005}
                onChange={(value) => patchCloud("cloudShadowOnSurfaceStrength", value)}
              />
            </>
          ) : null}
        </>
      ) : null}
    </PropertySection>
  );
}

export function WindSection({ ctx }: SectionProps) {
  const { env, defaults, editor, isModified } = ctx;
  const wind = env.wind;
  const patchWind = <K extends keyof Wind>(field: K, value: Wind[K]): void =>
    editor.patchBlock("wind", field, value);

  return (
    <PropertySection
      title="Wind"
      summary={`${wind.speed.toFixed(1)} m/s`}
      open={ctx.sectionOpen("wind")}
      onOpenChange={(open) => ctx.setSectionOpen("wind", open)}
      modified={defaults !== null && isModified(wind, defaults.wind)}
      onReset={() => ctx.resetNested("Reset wind", "wind")}
    >
      <NumberRow
        label="Wind Dir."
        editor={editor}
        value={wind.orientation}
        min={-360}
        max={360}
        step={1}
        onChange={(value) => patchWind("orientation", value)}
      />
      <NumberRow
        label="Wind Speed"
        editor={editor}
        value={wind.speed}
        min={0}
        max={200}
        step={0.1}
        onChange={(value) => patchWind("speed", value)}
      />
      <NumberRow
        label="Wind Gust"
        editor={editor}
        value={wind.gust}
        min={0}
        max={4}
        step={0.01}
        onChange={(value) => patchWind("gust", value)}
      />
      <NumberRow
        label="Turb. Octaves"
        editor={editor}
        value={wind.turbulenceOctaves}
        min={0}
        max={8}
        step={1}
        onChange={(value) => patchWind("turbulenceOctaves", Math.round(value))}
      />
      <NumberRow
        label="Turb. Roughness"
        editor={editor}
        value={wind.turbulenceRoughness}
        min={0}
        max={1}
        step={0.01}
        onChange={(value) => patchWind("turbulenceRoughness", value)}
      />
      <NumberRow
        label="Gust Freq. (Hz)"
        editor={editor}
        value={wind.gustFrequency}
        min={0}
        max={2}
        step={0.01}
        onChange={(value) => patchWind("gustFrequency", value)}
      />
      <NumberRow
        label="Ref. Height (m)"
        editor={editor}
        value={wind.referenceHeight}
        min={0.1}
        max={200}
        step={0.1}
        onChange={(value) => patchWind("referenceHeight", value)}
      />
      <NumberRow
        label="Height Exp."
        editor={editor}
        value={wind.heightExponent}
        min={0}
        max={1}
        step={0.01}
        onChange={(value) => patchWind("heightExponent", value)}
      />
    </PropertySection>
  );
}
