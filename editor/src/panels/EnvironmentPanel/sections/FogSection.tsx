import { PropertySection } from "../../../components/PropertySection";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import type { Environment, Vec3 } from "../../../protocol";
import { ColorRow, NumberRow, Row, SwitchRow } from "../Row";
import type { SectionProps } from "../sectionContext";

type Fog = Environment["fog"];

const FOG_MODES: { value: Fog["mode"]; label: string }[] = [
  { value: "analytic", label: "Analytic" },
  { value: "volumetric", label: "Volumetric" },
];

/// Renderer-cost knobs live in the Render panel, so they are excluded from the dirty mark and reset.
const FOG_COST_FIELDS = ["quality", "historyBlend", "neighborhoodClamp", "lightClamp"];

const authoredFogFields = (fog: Fog): (keyof Fog)[] =>
  (Object.keys(fog) as (keyof Fog)[]).filter((field) => !FOG_COST_FIELDS.includes(field));

export function FogSection({ ctx }: SectionProps) {
  const { env, defaults, allDetails, editor, isModified } = ctx;
  const fog = env.fog;
  const patchFog = <K extends keyof Fog>(field: K, value: Fog[K]): void =>
    editor.patchBlock("fog", field, value);
  const fogVec =
    (field: "albedo" | "emissive" | "directionalColor") =>
    (channels: Record<string, number>): void => {
      patchFog(field, { ...(fog[field] as Vec3), ...channels } as Fog[typeof field]);
    };

  return (
    <PropertySection
      title="Fog appearance"
      summary={fog.enabled ? fog.mode : "Disabled"}
      open={ctx.sectionOpen("fogAppearance")}
      onOpenChange={(open) => ctx.setSectionOpen("fogAppearance", open)}
      modified={
        defaults !== null &&
        authoredFogFields(fog).some((field) => isModified(fog[field], defaults.fog[field]))
      }
      onReset={() => ctx.resetNested("Reset fog", "fog", authoredFogFields(fog))}
    >
      <SwitchRow
        label="Fog"
        checked={fog.enabled}
        onCheckedChange={(checked) => patchFog("enabled", checked)}
      />

      {fog.enabled ? (
        <>
          <Row label="Mode">
            <Select
              value={fog.mode}
              onValueChange={(value) => patchFog("mode", value as Fog["mode"])}
            >
              <SelectTrigger size="sm" className="h-7 w-full font-mono text-[11px]">
                <SelectValue />
              </SelectTrigger>
              <SelectContent>
                {FOG_MODES.map((mode) => (
                  <SelectItem key={mode.value} value={mode.value} className="text-[11px]">
                    {mode.label}
                  </SelectItem>
                ))}
              </SelectContent>
            </Select>
          </Row>

          {fog.mode === "volumetric" ? (
            <>
              <NumberRow
                label="Base Density"
                editor={editor}
                value={fog.baseDensity}
                min={0}
                max={2}
                step={0.001}
                onChange={(v) => patchFog("baseDensity", v)}
              />
              <NumberRow
                label="Scatter Albedo"
                editor={editor}
                value={fog.scatterAlbedo}
                min={0}
                max={1}
                step={0.01}
                onChange={(v) => patchFog("scatterAlbedo", v)}
              />
              <NumberRow
                label="Phase g"
                editor={editor}
                value={fog.phaseG}
                min={-0.99}
                max={0.99}
                step={0.01}
                onChange={(v) => patchFog("phaseG", v)}
              />
            </>
          ) : null}

          <NumberRow
            label="Density"
            editor={editor}
            value={fog.density}
            min={0}
            max={2}
            step={0.001}
            onChange={(v) => patchFog("density", v)}
          />

          <ColorRow label="Albedo" editor={editor} value={fog.albedo} onChange={fogVec("albedo")} />

          {allDetails ? (
            <NumberRow
              label="Height"
              editor={editor}
              value={fog.height}
              min={-1000}
              max={1000}
              step={0.1}
              onChange={(v) => patchFog("height", v)}
            />
          ) : null}

          {allDetails ? (
            <NumberRow
              label="Height Falloff"
              editor={editor}
              value={fog.heightFalloff}
              min={0}
              max={5}
              step={0.005}
              onChange={(v) => patchFog("heightFalloff", v)}
            />
          ) : null}

          <NumberRow
            label="Start Dist."
            editor={editor}
            value={fog.startDistance}
            min={0}
            max={1000}
            step={0.1}
            onChange={(v) => patchFog("startDistance", v)}
          />

          <NumberRow
            label="Max Opacity"
            editor={editor}
            value={fog.maxOpacity}
            min={0}
            max={1}
            step={0.005}
            onChange={(v) => patchFog("maxOpacity", v)}
          />

          {allDetails ? (
            <>
              <ColorRow
                label="Emissive"
                editor={editor}
                value={fog.emissive}
                onChange={fogVec("emissive")}
              />
              <ColorRow
                label="Sun Color"
                editor={editor}
                value={fog.directionalColor}
                onChange={fogVec("directionalColor")}
              />
              <NumberRow
                label="Sun Exp."
                editor={editor}
                value={fog.directionalExponent}
                min={1}
                max={64}
                step={0.1}
                onChange={(v) => patchFog("directionalExponent", v)}
              />
              <NumberRow
                label="Ground Density"
                editor={editor}
                value={fog.layer2Density}
                min={0}
                max={2}
                step={0.001}
                onChange={(v) => patchFog("layer2Density", v)}
              />
              <NumberRow
                label="Ground Falloff"
                editor={editor}
                value={fog.layer2Falloff}
                min={0}
                max={5}
                step={0.005}
                onChange={(v) => patchFog("layer2Falloff", v)}
              />
              <NumberRow
                label="Ground Height"
                editor={editor}
                value={fog.layer2Height}
                min={-1000}
                max={1000}
                step={0.1}
                onChange={(v) => patchFog("layer2Height", v)}
              />
              <SwitchRow
                label="Aerial Persp."
                checked={fog.aerialPerspective}
                onCheckedChange={(checked) => patchFog("aerialPerspective", checked)}
              />
              {fog.aerialPerspective ? (
                <NumberRow
                  label="AP Intensity"
                  editor={editor}
                  value={fog.aerialIntensity}
                  min={0}
                  max={8}
                  step={0.05}
                  onChange={(v) => patchFog("aerialIntensity", v)}
                />
              ) : null}
            </>
          ) : null}
        </>
      ) : null}
    </PropertySection>
  );
}
