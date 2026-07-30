/// The Material editor: select a `.smat`, edit its standard or thin-sheet surface data, and render
/// a studio-lit preview through the control plane.
import { useCallback, useEffect, useRef, useState } from "react";
import { client } from "../../control/client";
import { useEditorStore } from "../../state/store";
import { renderField, type FieldRenderContext } from "../../components/fieldRenderer";
import { AssetPicker } from "../../components/AssetPicker";
import { NumberDrag } from "../../components/NumberDrag";
import { makeCoalescer, type Coalescer } from "../../control/coalesce";
import { errorText, notifyError } from "../../lib/flash";
import { humanizeFieldName } from "../../lib/humanize";
import {
  ColorParameter,
  CommitTextField,
  ParameterField,
  ParameterGroup,
  UnitParameter,
  splitHashes,
} from "./parameterFields";
import type { CommandResultMap } from "../../protocol";
import { Button } from "@/components/ui/button";
import { Label } from "@/components/ui/label";
import { Switch } from "@/components/ui/switch";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";

const FACTOR_FIELDS = [
  "baseColor",
  "metallic",
  "roughness",
  "emissive",
  "emissiveStrength",
] as const;
const TEXTURE_FIELDS = [
  "albedoTexture",
  "ormTexture",
  "normalTexture",
  "emissiveTexture",
  "heightTexture",
] as const;

type MaterialSurface = CommandResultMap["material-get"]["surface"];
type ThinSheetSurface = Extract<MaterialSurface, { model: "thin-sheet-foliage" }>;
type ThinSheetParameters = ThinSheetSurface["parameters"];

const UNIT_MAX = 65_535;
const FIXED_SCALE = 65_536;
const ZERO_HASH = "0".repeat(64);
const SOURCE_EXTENT_AXES = ["width", "height"] as const;
const NORMAL_SECOND_MOMENT_LABELS = ["XX", "YY", "ZZ", "XY", "XZ", "YZ"] as const;

const DEFAULT_THIN_SHEET_PARAMETERS: ThinSheetParameters = {
  frontAlbedoResponse: 30_000,
  backAlbedoResponse: 30_000,
  thicknessBits: 655,
  absorptionColorBits: [0, 0, 0],
  transmissionColorBits: [30_000, 30_000, 30_000],
  roughness: 32_768,
  normalBehavior: "face-forward-back",
  coverageSource: { kind: "albedo-alpha" },
  coverage: {
    referenceCutoff: 32_768,
    sourceExtent: [1, 1],
    spatialHashSalt: "1",
    classification: "masked",
    mipHashes: [],
  },
  voxelMoments: {
    occupancy: 0,
    albedoMeanBits: [0, 0, 0],
    roughnessMean: 0,
    transmissionMeanBits: [0, 0, 0],
    thicknessMeanBits: 0,
    normalSecondMomentsBits: [0, 0, 0, 0, 0, 0],
  },
  opacityMicromap: {
    enabled: false,
    maxSubdivision: 0,
    transparentThreshold: 0,
    opaqueThreshold: 0,
  },
  energyLimit: UNIT_MAX,
};

// The height-map technique (one grayscale Height Map, the mode picks how it is realized — the
// shape Unity HDRP / Godot / Blender converge on). Shown only when a Height Map is assigned.
const HEIGHT_MODES = [
  { value: "bump", label: "Bump", hint: "Shading bump only — flat silhouette (safe default)" },
  { value: "parallax", label: "Parallax", hint: "Parallax occlusion mapping — fake depth" },
  {
    value: "displacement",
    label: "Displacement",
    hint: "Real geometry — true silhouette (needs a dense mesh)",
  },
] as const;

interface MaterialRef {
  id: string;
  name: string;
}

interface MaterialEditorPanelProps {
  /// When set, the panel edits this material and ignores the store's `selectedMaterialId` — the
  /// asset previewer pins it to the previewed material so its sidebar edits that subject, not the
  /// scene selection.
  pinnedMaterialId?: string;
  /// Hide the top material-selector row (dropdown + New/Graph): the subject is fixed.
  hideSelector?: boolean;
  /// Hide the mini preview sphere (and skip its GPU render): the asset previewer already shows the
  /// material full-size in the viewport, so the inline thumbnail is redundant.
  hidePreview?: boolean;
}

export function MaterialEditorPanel({
  pinnedMaterialId,
  hideSelector = false,
  hidePreview = false,
}: MaterialEditorPanelProps = {}) {
  const selectedMaterialId = useEditorStore((s) => s.selectedMaterialId);
  const setSelectedMaterialId = useEditorStore((s) => s.setSelectedMaterialId);
  const setDragActive = useEditorStore((s) => s.setDragActive);
  const openMaterialGraphTab = useEditorStore((s) => s.openMaterialGraphTab);
  // The material this panel edits: a pinned subject (the previewer) wins over the scene selection.
  // Everything below keys off this; the selector row only drives the store's `selectedMaterialId`.
  const activeMaterialId = pinnedMaterialId ?? selectedMaterialId;

  const [materials, setMaterials] = useState<MaterialRef[]>([]);
  const [fields, setFields] = useState<Record<string, unknown> | null>(null);
  const [preview, setPreview] = useState<string | null>(null);
  const coalescers = useRef<Map<string, Coalescer<unknown>>>(new Map());
  // One preview render per edit-burst (not one per field): edits push the material id here and the
  // coalescer keeps at most one preview-render in flight, re-driving with the latest on completion.
  const previewCoalescer = useRef<Coalescer<string> | null>(null);
  if (previewCoalescer.current === null) {
    previewCoalescer.current = makeCoalescer<string>({
      throttleMs: 200,
      send: async (id) => {
        const result = await client.previewRender(id, 256);
        setPreview(result.png);
      },
    });
  }

  const refreshList = useCallback(async () => {
    try {
      const result = await client.materialList();
      setMaterials(result.materials.map((m) => ({ id: m.id, name: m.name })));
    } catch (err) {
      notifyError(errorText(err));
    }
  }, []);

  useEffect(() => {
    // The list only feeds the selector; skip the fetch when the selector is hidden (pinned subject).
    if (hideSelector) {
      return;
    }
    void refreshList();
  }, [refreshList, hideSelector]);

  useEffect(() => {
    coalescers.current.clear();
    if (!activeMaterialId) {
      setFields(null);
      setPreview(null);
      return;
    }
    const id = activeMaterialId;
    void (async () => {
      try {
        const material = await client.materialGet(id);
        setFields(material as unknown as Record<string, unknown>);
      } catch (err) {
        notifyError(errorText(err));
      }
    })();
    if (!hidePreview) {
      previewCoalescer.current?.push(id);
    }
  }, [activeMaterialId, hidePreview]);

  const editField = useCallback(
    (field: string, value: unknown) => {
      if (!activeMaterialId) {
        return;
      }
      const id = activeMaterialId;
      setFields((current) => (current ? { ...current, [field]: value } : current));
      let coalescer = coalescers.current.get(field);
      if (!coalescer) {
        coalescer = makeCoalescer<unknown>({
          send: async (latest) => {
            try {
              const patch = { [field]: latest } as Parameters<typeof client.materialUpdate>[1];
              await client.materialUpdate(id, patch);
              if (!hidePreview) {
                previewCoalescer.current?.push(id);
              }
            } catch (err) {
              notifyError(errorText(err));
            }
          },
        });
        coalescers.current.set(field, coalescer);
      }
      coalescer.push(value);
    },
    [activeMaterialId, hidePreview],
  );

  const newMaterial = useCallback(async () => {
    try {
      const created = await client.materialCreate("Material");
      await refreshList();
      setSelectedMaterialId(created.id);
    } catch (err) {
      notifyError(errorText(err));
    }
  }, [refreshList, setSelectedMaterialId]);

  const ctx: FieldRenderContext = {
    onDragStart: () => setDragActive(true),
    onDragEnd: () => setDragActive(false),
  };
  const surface = fields?.surface as MaterialSurface | undefined;

  return (
    <div className="flex h-full flex-col gap-3 overflow-y-auto bg-background p-3 text-[12px] text-foreground">
      {hideSelector ? null : (
        <div className="flex items-center gap-2">
          <Select
            value={selectedMaterialId ?? ""}
            onValueChange={(value) => setSelectedMaterialId(value || null)}
          >
            <SelectTrigger size="sm" className="h-7 w-full text-[11px]">
              <SelectValue placeholder="Select a material…" />
            </SelectTrigger>
            <SelectContent>
              {materials.map((m) => (
                <SelectItem key={m.id} value={m.id} className="text-[11px]">
                  {m.name}
                </SelectItem>
              ))}
            </SelectContent>
          </Select>
          <Button size="sm" onClick={() => void newMaterial()}>
            New
          </Button>
          <Button
            size="sm"
            variant="secondary"
            disabled={!selectedMaterialId}
            onClick={() => {
              if (selectedMaterialId) {
                openMaterialGraphTab(selectedMaterialId);
              }
            }}
          >
            Graph
          </Button>
        </div>
      )}

      {hidePreview ? null : preview ? (
        <img
          src={`data:image/png;base64,${preview}`}
          alt="material preview"
          className="aspect-square w-full rounded border border-border object-cover"
        />
      ) : (
        <div className="flex aspect-square w-full items-center justify-center rounded border border-dashed border-border text-muted-foreground">
          {activeMaterialId ? "Rendering…" : "No material selected"}
        </div>
      )}

      {surface ? (
        <SurfaceEditor
          surface={surface}
          onChange={(next) => editField("surface", next)}
          onDragStart={ctx.onDragStart}
          onDragEnd={ctx.onDragEnd}
        />
      ) : null}

      {fields
        ? FACTOR_FIELDS.map((field) => (
            <div key={field} className="flex flex-col gap-1">
              <Label className="text-[11px] text-muted-foreground">
                {humanizeFieldName(field)}
              </Label>
              {renderField("Material", field, fields[field], (next) => editField(field, next), ctx)}
            </div>
          ))
        : null}

      {fields
        ? TEXTURE_FIELDS.map((field) => (
            <div key={field} className="flex flex-col gap-1">
              <Label className="text-[11px] text-muted-foreground">
                {humanizeFieldName(field)}
              </Label>
              {renderField("Material", field, fields[field], (next) => editField(field, next), ctx)}
            </div>
          ))
        : null}

      {fields && String(fields.heightTexture ?? "0") !== "0" ? (
        <>
          <div className="flex flex-col gap-1">
            <Label className="text-[11px] text-muted-foreground">Height mode</Label>
            <Select
              value={String(fields.heightMode ?? "bump")}
              onValueChange={(value) => editField("heightMode", value)}
            >
              <SelectTrigger size="sm" className="h-7 w-full text-[11px]">
                <SelectValue />
              </SelectTrigger>
              <SelectContent>
                {HEIGHT_MODES.map((mode) => (
                  <SelectItem key={mode.value} value={mode.value} className="text-[11px]">
                    {mode.label}
                  </SelectItem>
                ))}
              </SelectContent>
            </Select>
            <p className="text-[10px] text-muted-foreground">
              {HEIGHT_MODES.find((m) => m.value === String(fields.heightMode ?? "bump"))?.hint}
            </p>
          </div>
          <div className="flex flex-col gap-1">
            <Label className="text-[11px] text-muted-foreground">
              {humanizeFieldName("heightScale")}
            </Label>
            {renderField(
              "Material",
              "heightScale",
              fields.heightScale,
              (next) => editField("heightScale", next),
              ctx,
            )}
          </div>
          {String(fields.heightMode ?? "bump") === "displacement" ? (
            <div className="flex flex-col gap-1">
              <Label className="text-[11px] text-muted-foreground">
                {humanizeFieldName("vectorDisplacementTexture")}
              </Label>
              {renderField(
                "Material",
                "vectorDisplacementTexture",
                fields.vectorDisplacementTexture,
                (next) => editField("vectorDisplacementTexture", next),
                ctx,
              )}
              <p className="text-[10px] text-muted-foreground">
                Tangent-space XYZ map — offsets vertices in any direction for overhangs (scalar
                height along the normal when unset).
              </p>
            </div>
          ) : null}
        </>
      ) : null}
    </div>
  );
}

interface SurfaceEditorProps {
  surface: MaterialSurface;
  onChange(surface: MaterialSurface): void;
  onDragStart(): void;
  onDragEnd(): void;
}

function SurfaceEditor({ surface, onChange, onDragStart, onDragEnd }: SurfaceEditorProps) {
  const setModel = (model: MaterialSurface["model"]): void => {
    onChange(
      model === "standard"
        ? { model: "standard" }
        : { model: "thin-sheet-foliage", parameters: DEFAULT_THIN_SHEET_PARAMETERS },
    );
  };

  return (
    <section className="flex flex-col gap-3 rounded-md border border-border bg-card p-3">
      <ParameterField label="Surface model">
        <Select
          value={surface.model}
          onValueChange={(value) => setModel(value as MaterialSurface["model"])}
        >
          <SelectTrigger size="sm" className="h-7 w-full text-[11px]">
            <SelectValue />
          </SelectTrigger>
          <SelectContent>
            <SelectItem value="standard" className="text-[11px]">
              Standard
            </SelectItem>
            <SelectItem value="thin-sheet-foliage" className="text-[11px]">
              Thin-sheet foliage
            </SelectItem>
          </SelectContent>
        </Select>
      </ParameterField>
      {surface.model === "thin-sheet-foliage" ? (
        <ThinSheetEditor
          parameters={surface.parameters}
          onChange={(parameters) => onChange({ model: "thin-sheet-foliage", parameters })}
          onDragStart={onDragStart}
          onDragEnd={onDragEnd}
        />
      ) : null}
    </section>
  );
}

interface ThinSheetEditorProps {
  parameters: ThinSheetParameters;
  onChange(parameters: ThinSheetParameters): void;
  onDragStart(): void;
  onDragEnd(): void;
}

function ThinSheetEditor({ parameters, onChange, onDragStart, onDragEnd }: ThinSheetEditorProps) {
  const assets = useEditorStore((state) => state.assets);
  const textures = assets.filter((asset) => asset.type === "texture");
  const patch = (next: Partial<ThinSheetParameters>): void => onChange({ ...parameters, ...next });
  const maxTransmission = Math.max(...parameters.transmissionColorBits);
  const responseMax = Math.max(0, parameters.energyLimit - maxTransmission);
  const maxResponse = Math.max(parameters.frontAlbedoResponse, parameters.backAlbedoResponse);
  const transmissionMax = Math.max(0, parameters.energyLimit - maxResponse);
  const minimumEnergy = Math.min(UNIT_MAX, maxResponse + maxTransmission);

  const setCoverageSource = (kind: ThinSheetParameters["coverageSource"]["kind"]): void => {
    if (kind === "albedo-alpha") {
      patch({ coverageSource: { kind: "albedo-alpha" } });
      return;
    }
    if (kind === "modeled-geometry") {
      patch({ coverageSource: { kind: "modeled-geometry" } });
      return;
    }
    const current =
      parameters.coverageSource.kind === "texture" ? parameters.coverageSource.texture : null;
    const texture = current ?? textures[0]?.id;
    if (!texture) {
      notifyError("A dedicated coverage source needs a texture asset");
      return;
    }
    patch({ coverageSource: { kind: "texture", texture } });
  };

  return (
    <div className="flex flex-col gap-4">
      <ParameterGroup title="Optical response">
        <UnitParameter
          label="Front albedo response"
          bits={parameters.frontAlbedoResponse}
          maxBits={responseMax}
          onChange={(frontAlbedoResponse) => patch({ frontAlbedoResponse })}
          onDragStart={onDragStart}
          onDragEnd={onDragEnd}
        />
        <UnitParameter
          label="Back albedo response"
          bits={parameters.backAlbedoResponse}
          maxBits={responseMax}
          onChange={(backAlbedoResponse) => patch({ backAlbedoResponse })}
          onDragStart={onDragStart}
          onDragEnd={onDragEnd}
        />
        <ColorParameter
          label="Absorption color"
          bits={parameters.absorptionColorBits}
          onChange={(absorptionColorBits) => patch({ absorptionColorBits })}
          onDragStart={onDragStart}
          onDragEnd={onDragEnd}
        />
        <ColorParameter
          label="Transmission color"
          bits={parameters.transmissionColorBits}
          maxBits={transmissionMax}
          onChange={(transmissionColorBits) => patch({ transmissionColorBits })}
          onDragStart={onDragStart}
          onDragEnd={onDragEnd}
        />
        <UnitParameter
          label="Roughness"
          bits={parameters.roughness}
          onChange={(roughness) => patch({ roughness })}
          onDragStart={onDragStart}
          onDragEnd={onDragEnd}
        />
        <ParameterField label="Thickness (m)">
          <NumberDrag
            value={parameters.thicknessBits / FIXED_SCALE}
            min={1 / FIXED_SCALE}
            step={0.001}
            onChange={(value) =>
              patch({ thicknessBits: Math.max(1, Math.round(value * FIXED_SCALE)) })
            }
            onDragStart={onDragStart}
            onDragEnd={onDragEnd}
          />
        </ParameterField>
        <ParameterField label="Normal behavior">
          <Select
            value={parameters.normalBehavior}
            onValueChange={(normalBehavior) =>
              patch({ normalBehavior: normalBehavior as ThinSheetParameters["normalBehavior"] })
            }
          >
            <SelectTrigger size="sm" className="h-7 w-full text-[11px]">
              <SelectValue />
            </SelectTrigger>
            <SelectContent>
              <SelectItem value="preserve" className="text-[11px]">
                Preserve
              </SelectItem>
              <SelectItem value="face-forward-back" className="text-[11px]">
                Face forward on back face
              </SelectItem>
              <SelectItem value="symmetric" className="text-[11px]">
                Symmetric
              </SelectItem>
            </SelectContent>
          </Select>
        </ParameterField>
        <UnitParameter
          label="Energy limit"
          bits={parameters.energyLimit}
          minBits={minimumEnergy}
          onChange={(energyLimit) => patch({ energyLimit })}
          onDragStart={onDragStart}
          onDragEnd={onDragEnd}
        />
      </ParameterGroup>

      <ParameterGroup title="Coverage">
        <ParameterField label="Coverage source">
          <Select
            value={parameters.coverageSource.kind}
            onValueChange={(value) =>
              setCoverageSource(value as ThinSheetParameters["coverageSource"]["kind"])
            }
          >
            <SelectTrigger size="sm" className="h-7 w-full text-[11px]">
              <SelectValue />
            </SelectTrigger>
            <SelectContent>
              <SelectItem value="albedo-alpha" className="text-[11px]">
                Albedo alpha
              </SelectItem>
              <SelectItem value="texture" className="text-[11px]">
                Dedicated texture
              </SelectItem>
              <SelectItem value="modeled-geometry" className="text-[11px]">
                Modeled geometry
              </SelectItem>
            </SelectContent>
          </Select>
        </ParameterField>
        {parameters.coverageSource.kind === "texture" ? (
          <ParameterField label="Coverage texture">
            <AssetPicker
              value={parameters.coverageSource.texture}
              assetType="texture"
              onChange={(texture) => {
                if (texture === "0") {
                  notifyError("A dedicated coverage source cannot be empty");
                  return;
                }
                patch({ coverageSource: { kind: "texture", texture } });
              }}
            />
          </ParameterField>
        ) : null}
        <UnitParameter
          label="Reference cutoff"
          bits={parameters.coverage.referenceCutoff}
          onChange={(referenceCutoff) =>
            patch({ coverage: { ...parameters.coverage, referenceCutoff } })
          }
          onDragStart={onDragStart}
          onDragEnd={onDragEnd}
        />
        <ParameterField label="Source extent">
          <div className="grid grid-cols-2 gap-2">
            {SOURCE_EXTENT_AXES.map((axis, index) => (
              <NumberDrag
                key={axis}
                value={parameters.coverage.sourceExtent[index]}
                min={1}
                max={4_294_967_295}
                step={1}
                onChange={(next) => {
                  const sourceExtent = [...parameters.coverage.sourceExtent] as [number, number];
                  sourceExtent[index] = Math.round(next);
                  patch({ coverage: { ...parameters.coverage, sourceExtent } });
                }}
                onDragStart={onDragStart}
                onDragEnd={onDragEnd}
              />
            ))}
          </div>
        </ParameterField>
        <ParameterField label="Spatial hash salt">
          <CommitTextField
            value={parameters.coverage.spatialHashSalt}
            inputMode="numeric"
            validate={(value) => /^[1-9][0-9]*$/.test(value)}
            validationMessage="Spatial hash salt must be a positive decimal integer"
            onCommit={(spatialHashSalt) =>
              patch({ coverage: { ...parameters.coverage, spatialHashSalt } })
            }
          />
        </ParameterField>
        <ParameterField label="Alpha classification">
          <Select
            value={parameters.coverage.classification}
            onValueChange={(classification) =>
              patch({
                coverage: {
                  ...parameters.coverage,
                  classification:
                    classification as ThinSheetParameters["coverage"]["classification"],
                },
              })
            }
          >
            <SelectTrigger size="sm" className="h-7 w-full text-[11px]">
              <SelectValue />
            </SelectTrigger>
            <SelectContent>
              <SelectItem value="opaque" className="text-[11px]">
                Opaque
              </SelectItem>
              <SelectItem value="masked" className="text-[11px]">
                Masked
              </SelectItem>
              <SelectItem value="transmissive" className="text-[11px]">
                Transmissive
              </SelectItem>
            </SelectContent>
          </Select>
        </ParameterField>
        <ParameterField label="Coverage mip hashes">
          <CommitTextField
            value={parameters.coverage.mipHashes.join(", ")}
            placeholder={ZERO_HASH}
            validate={(value) =>
              value.length === 0 || splitHashes(value).every((hash) => /^[0-9a-f]{64}$/.test(hash))
            }
            validationMessage="Coverage hashes must be lowercase 64-digit SHA-256 values"
            onCommit={(value) =>
              patch({ coverage: { ...parameters.coverage, mipHashes: splitHashes(value) } })
            }
          />
        </ParameterField>
      </ParameterGroup>

      <VoxelMomentsEditor
        parameters={parameters}
        onChange={patch}
        onDragStart={onDragStart}
        onDragEnd={onDragEnd}
      />
      <OpacityMicromapEditor
        parameters={parameters}
        onChange={patch}
        onDragStart={onDragStart}
        onDragEnd={onDragEnd}
      />
    </div>
  );
}

function VoxelMomentsEditor({
  parameters,
  onChange,
  onDragStart,
  onDragEnd,
}: {
  parameters: ThinSheetParameters;
  onChange(patch: Partial<ThinSheetParameters>): void;
  onDragStart(): void;
  onDragEnd(): void;
}) {
  const moments = parameters.voxelMoments;
  const patch = (next: Partial<ThinSheetParameters["voxelMoments"]>): void =>
    onChange({ voxelMoments: { ...moments, ...next } });
  return (
    <ParameterGroup title="Voxel material moments">
      <UnitParameter
        label="Occupancy"
        bits={moments.occupancy}
        onChange={(occupancy) => patch({ occupancy })}
        onDragStart={onDragStart}
        onDragEnd={onDragEnd}
      />
      <ColorParameter
        label="Mean albedo"
        bits={moments.albedoMeanBits}
        onChange={(albedoMeanBits) => patch({ albedoMeanBits })}
        onDragStart={onDragStart}
        onDragEnd={onDragEnd}
      />
      <UnitParameter
        label="Mean roughness"
        bits={moments.roughnessMean}
        onChange={(roughnessMean) => patch({ roughnessMean })}
        onDragStart={onDragStart}
        onDragEnd={onDragEnd}
      />
      <ColorParameter
        label="Mean transmission"
        bits={moments.transmissionMeanBits}
        onChange={(transmissionMeanBits) => patch({ transmissionMeanBits })}
        onDragStart={onDragStart}
        onDragEnd={onDragEnd}
      />
      <ParameterField label="Mean thickness (m)">
        <NumberDrag
          value={moments.thicknessMeanBits / FIXED_SCALE}
          step={0.001}
          onChange={(value) => patch({ thicknessMeanBits: Math.round(value * FIXED_SCALE) })}
          onDragStart={onDragStart}
          onDragEnd={onDragEnd}
        />
      </ParameterField>
      <ParameterField label="Normal second moments">
        <div className="grid grid-cols-3 gap-1.5">
          {NORMAL_SECOND_MOMENT_LABELS.map((label, index) => (
            <div key={label} className="flex min-w-0 flex-col gap-1">
              <span className="text-[9px] text-muted-foreground">{label}</span>
              <NumberDrag
                value={moments.normalSecondMomentsBits[index] / FIXED_SCALE}
                step={0.01}
                onChange={(value) => {
                  const normalSecondMomentsBits = [...moments.normalSecondMomentsBits] as [
                    number,
                    number,
                    number,
                    number,
                    number,
                    number,
                  ];
                  normalSecondMomentsBits[index] = Math.round(value * FIXED_SCALE);
                  patch({ normalSecondMomentsBits });
                }}
                onDragStart={onDragStart}
                onDragEnd={onDragEnd}
              />
            </div>
          ))}
        </div>
      </ParameterField>
    </ParameterGroup>
  );
}

function OpacityMicromapEditor({
  parameters,
  onChange,
  onDragStart,
  onDragEnd,
}: {
  parameters: ThinSheetParameters;
  onChange(patch: Partial<ThinSheetParameters>): void;
  onDragStart(): void;
  onDragEnd(): void;
}) {
  const omm = parameters.opacityMicromap;
  const patch = (next: Partial<ThinSheetParameters["opacityMicromap"]>): void =>
    onChange({ opacityMicromap: { ...omm, ...next } });
  return (
    <ParameterGroup title="Opacity micromap derivation">
      <ParameterField label="Enabled" inline>
        <Switch checked={omm.enabled} onCheckedChange={(enabled) => patch({ enabled })} />
      </ParameterField>
      <ParameterField label="Maximum subdivision">
        <NumberDrag
          value={omm.maxSubdivision}
          min={0}
          max={255}
          step={1}
          onChange={(maxSubdivision) => patch({ maxSubdivision: Math.round(maxSubdivision) })}
          onDragStart={onDragStart}
          onDragEnd={onDragEnd}
        />
      </ParameterField>
      <UnitParameter
        label="Transparent threshold"
        bits={omm.transparentThreshold}
        maxBits={omm.opaqueThreshold}
        onChange={(transparentThreshold) => patch({ transparentThreshold })}
        onDragStart={onDragStart}
        onDragEnd={onDragEnd}
      />
      <UnitParameter
        label="Opaque threshold"
        bits={omm.opaqueThreshold}
        minBits={omm.transparentThreshold}
        onChange={(opaqueThreshold) => patch({ opaqueThreshold })}
        onDragStart={onDragStart}
        onDragEnd={onDragEnd}
      />
    </ParameterGroup>
  );
}
