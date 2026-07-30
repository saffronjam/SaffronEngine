import { AssetPicker } from "../../../components/AssetPicker";
import { PropertySection } from "../../../components/PropertySection";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { DEG_TO_RAD, RAD_TO_DEG } from "@/lib/utils";
import type { Environment, Vec3 } from "../../../protocol";
import { ColorRow, NumberRow, Row, SwitchRow, VectorRow } from "../Row";
import type { SectionProps } from "../sectionContext";

type SkyMode = Environment["skyMode"];
type Atmosphere = Environment["atmosphere"];

const SKY_MODES: { value: SkyMode; label: string }[] = [
  { value: "color", label: "Color" },
  { value: "texture", label: "Texture" },
  { value: "procedural", label: "Procedural" },
];

const BACKGROUND_FIELDS = [
  "skyMode",
  "clearColor",
  "skyTexture",
  "skyIntensity",
  "skyRotation",
  "visible",
] as const;

const LIGHTING_FIELDS = ["useSkyForAmbient", "ambientColor", "ambientIntensity"] as const;

const ATMOSPHERE_FIELDS = [
  "enabled",
  "planetRadius",
  "atmosphereHeight",
  "rayleighScattering",
  "rayleighScaleHeight",
  "mieScattering",
  "mieScaleHeight",
  "mieAnisotropy",
  "ozoneAbsorption",
  "sunDiskAngularRadius",
  "sunDiskIntensity",
  "moonDiskAngularRadius",
  "moonDiskIntensity",
  "moonEarthshine",
] as const;

/// `skyRotation` is RADIANS on the wire and DEGREES in the UI; the conversion happens only at this
/// widget boundary.
export function BackgroundSection({ ctx }: SectionProps) {
  const { env, defaults, editor, isModified } = ctx;
  const patch = editor.patch;
  const vecChannel =
    (field: "clearColor" | "ambientColor") =>
    (channels: Record<string, number>): void => {
      patch(field, { ...(env[field] as Vec3), ...channels } as Vec3);
    };

  return (
    <PropertySection
      title="Background"
      summary={`${env.skyMode} · ${env.skyIntensity.toFixed(2)}×`}
      open={ctx.sectionOpen("background")}
      onOpenChange={(open) => ctx.setSectionOpen("background", open)}
      modified={
        defaults !== null &&
        BACKGROUND_FIELDS.some((field) => isModified(env[field], defaults[field]))
      }
      onReset={() => ctx.resetTopLevel("Reset sky background", [...BACKGROUND_FIELDS])}
    >
      <Row label="Sky Mode">
        <Select value={env.skyMode} onValueChange={(value) => patch("skyMode", value as SkyMode)}>
          <SelectTrigger size="sm" className="h-7 w-full font-mono text-[11px]">
            <SelectValue />
          </SelectTrigger>
          <SelectContent>
            {SKY_MODES.map((mode) => (
              <SelectItem key={mode.value} value={mode.value} className="text-[11px]">
                {mode.label}
              </SelectItem>
            ))}
          </SelectContent>
        </Select>
      </Row>

      {env.skyMode === "color" ? (
        <ColorRow
          label="Clear Color"
          editor={editor}
          value={env.clearColor}
          onChange={vecChannel("clearColor")}
        />
      ) : null}

      {env.skyMode === "texture" ? (
        <Row label="Sky Texture">
          <AssetPicker
            value={env.skyTexture}
            assetType="texture"
            onChange={(id) => patch("skyTexture", id)}
          />
        </Row>
      ) : null}

      <NumberRow
        label="Intensity"
        editor={editor}
        value={env.skyIntensity}
        min={0}
        max={100}
        step={0.01}
        onChange={(v) => patch("skyIntensity", v)}
      />

      {env.skyMode !== "color" ? (
        <NumberRow
          label="Rotation (°)"
          editor={editor}
          value={env.skyRotation * RAD_TO_DEG}
          min={-360}
          max={360}
          step={0.5}
          onChange={(deg) => patch("skyRotation", deg * DEG_TO_RAD)}
        />
      ) : null}

      <SwitchRow
        label="Visible"
        checked={env.visible}
        onCheckedChange={(checked) => patch("visible", checked)}
      />
    </PropertySection>
  );
}

export function LightingSection({ ctx }: SectionProps) {
  const { env, defaults, editor, isModified } = ctx;
  const patch = editor.patch;

  return (
    <PropertySection
      title="Environment lighting"
      summary={env.useSkyForAmbient ? "Authored fallback" : "Directional fallback"}
      open={ctx.sectionOpen("lighting")}
      onOpenChange={(open) => ctx.setSectionOpen("lighting", open)}
      modified={
        defaults !== null &&
        LIGHTING_FIELDS.some((field) => isModified(env[field], defaults[field]))
      }
      onReset={() => ctx.resetTopLevel("Reset environment lighting", [...LIGHTING_FIELDS])}
    >
      <Row label="Fallback source">
        <Select
          value={env.useSkyForAmbient ? "authored" : "directional"}
          onValueChange={(value) => patch("useSkyForAmbient", value === "authored")}
        >
          <SelectTrigger size="sm" className="h-7 w-full text-[11px]">
            <SelectValue />
          </SelectTrigger>
          <SelectContent>
            <SelectItem value="authored" className="text-[11px]">
              Authored color
            </SelectItem>
            <SelectItem value="directional" className="text-[11px]">
              Directional light
            </SelectItem>
          </SelectContent>
        </Select>
      </Row>

      {env.useSkyForAmbient ? (
        <>
          <ColorRow
            label="Ambient Color"
            editor={editor}
            value={env.ambientColor}
            onChange={(channels) =>
              patch("ambientColor", { ...(env.ambientColor as Vec3), ...channels } as Vec3)
            }
          />
          <NumberRow
            label="Ambient Int."
            editor={editor}
            value={env.ambientIntensity}
            min={0}
            max={10}
            step={0.005}
            onChange={(v) => patch("ambientIntensity", v)}
          />
        </>
      ) : null}
    </PropertySection>
  );
}

export function AtmosphereSection({ ctx }: SectionProps) {
  const { env, defaults, allDetails, editor, isModified } = ctx;
  const atmos = env.atmosphere;
  const patchAtmos = <K extends keyof Atmosphere>(field: K, value: Atmosphere[K]): void =>
    editor.patchBlock("atmosphere", field, value);
  const atmosVec =
    (field: "rayleighScattering" | "ozoneAbsorption") =>
    (channels: Record<string, number>): void => {
      patchAtmos(field, { ...(atmos[field] as Vec3), ...channels } as Atmosphere[typeof field]);
    };

  return (
    <PropertySection
      title="Physical atmosphere"
      summary={atmos.enabled ? "Enabled" : "Disabled"}
      open={ctx.sectionOpen("atmosphere")}
      onOpenChange={(open) => ctx.setSectionOpen("atmosphere", open)}
      modified={
        defaults !== null &&
        ATMOSPHERE_FIELDS.some((field) => isModified(atmos[field], defaults.atmosphere[field]))
      }
      onReset={() =>
        ctx.resetNested("Reset physical atmosphere", "atmosphere", [...ATMOSPHERE_FIELDS])
      }
    >
      <SwitchRow
        label="Atmosphere"
        checked={atmos.enabled}
        onCheckedChange={(checked) => patchAtmos("enabled", checked)}
      />

      {atmos.enabled ? (
        <>
          {allDetails ? (
            <>
              <NumberRow
                label="Planet radius"
                editor={editor}
                value={atmos.planetRadius}
                min={100}
                max={100000}
                step={1}
                onChange={(v) => patchAtmos("planetRadius", v)}
              />
              <NumberRow
                label="Atmos. height"
                editor={editor}
                value={atmos.atmosphereHeight}
                min={1}
                max={1000}
                step={1}
                onChange={(v) => patchAtmos("atmosphereHeight", v)}
              />
              <VectorRow
                label="Rayleigh"
                editor={editor}
                axes={["x", "y", "z"]}
                labels={["R", "G", "B"]}
                value={atmos.rayleighScattering as unknown as Record<string, number>}
                step={0.1}
                onChange={atmosVec("rayleighScattering")}
              />
              <NumberRow
                label="Rayleigh Ht."
                editor={editor}
                value={atmos.rayleighScaleHeight}
                min={0.1}
                max={60}
                step={0.1}
                onChange={(v) => patchAtmos("rayleighScaleHeight", v)}
              />
            </>
          ) : null}

          <NumberRow
            label="Mie"
            editor={editor}
            value={atmos.mieScattering}
            min={0}
            max={50}
            step={0.01}
            onChange={(v) => patchAtmos("mieScattering", v)}
          />

          {allDetails ? (
            <NumberRow
              label="Mie Ht."
              editor={editor}
              value={atmos.mieScaleHeight}
              min={0.1}
              max={20}
              step={0.05}
              onChange={(v) => patchAtmos("mieScaleHeight", v)}
            />
          ) : null}

          <NumberRow
            label="Mie Aniso."
            editor={editor}
            value={atmos.mieAnisotropy}
            min={-0.99}
            max={0.99}
            step={0.005}
            onChange={(v) => patchAtmos("mieAnisotropy", v)}
          />

          {allDetails ? (
            <VectorRow
              label="Ozone"
              editor={editor}
              axes={["x", "y", "z"]}
              labels={["R", "G", "B"]}
              value={atmos.ozoneAbsorption as unknown as Record<string, number>}
              step={0.01}
              onChange={atmosVec("ozoneAbsorption")}
            />
          ) : null}

          {allDetails ? (
            <NumberRow
              label="Sun radius"
              editor={editor}
              value={atmos.sunDiskAngularRadius}
              min={0.0001}
              max={0.05}
              step={0.00001}
              onChange={(v) => patchAtmos("sunDiskAngularRadius", v)}
            />
          ) : null}

          <NumberRow
            label="Sun Disk"
            editor={editor}
            value={atmos.sunDiskIntensity}
            min={0}
            max={100}
            step={0.1}
            onChange={(v) => patchAtmos("sunDiskIntensity", v)}
          />

          {allDetails ? (
            <NumberRow
              label="Moon Radius"
              editor={editor}
              value={atmos.moonDiskAngularRadius}
              min={0.0001}
              max={0.05}
              step={0.00001}
              onChange={(v) => patchAtmos("moonDiskAngularRadius", v)}
            />
          ) : null}

          <NumberRow
            label="Moon Disk"
            editor={editor}
            value={atmos.moonDiskIntensity}
            min={0}
            max={100}
            step={0.01}
            onChange={(v) => patchAtmos("moonDiskIntensity", v)}
          />

          {allDetails ? (
            <NumberRow
              label="Earthshine"
              editor={editor}
              value={atmos.moonEarthshine}
              min={0}
              max={1}
              step={0.001}
              onChange={(v) => patchAtmos("moonEarthshine", v)}
            />
          ) : null}
        </>
      ) : null}
    </PropertySection>
  );
}
