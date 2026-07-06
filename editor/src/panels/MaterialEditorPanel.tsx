/// The Material editor: pick a .smat material asset, see it on a studio-lit preview sphere, and
/// edit its factors live. Reads/writes over the control plane (material-list/get/update/create +
/// preview-render); edits are coalesced and re-render the preview. Texture-slot picking is the
/// entity inspector's job (assign-asset); this panel edits the shared material asset's factors.
import { useCallback, useEffect, useRef, useState } from "react";
import { client } from "../control/client";
import { useEditorStore } from "../state/store";
import { renderField, type FieldRenderContext } from "../components/fieldRenderer";
import { makeCoalescer, type Coalescer } from "../control/coalesce";
import { errorText, notifyError } from "../lib/flash";
import { humanizeFieldName } from "../lib/humanize";
import { Button } from "@/components/ui/button";
import { Label } from "@/components/ui/label";
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
            const patch = { [field]: latest } as Parameters<typeof client.materialUpdate>[1];
            await client.materialUpdate(id, patch);
            if (!hidePreview) {
              previewCoalescer.current?.push(id);
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
