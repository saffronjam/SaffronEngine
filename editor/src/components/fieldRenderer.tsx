/// Field-kind dispatcher: maps a `(component, field, value)` to a typed widget.
/// There is deliberately NO per-component switch — the panel iterates whatever
/// `inspect` returns and this resolver picks a widget by (1) the explicit
/// `FIELD_HINTS` parity table for the known components, else (2) the value's shape
/// ({x,y,z}->vec3, {x,y,z,w}->vec4, number/boolean/string), else (3) a read-only
/// text fallback so an unmapped field is still visible. So a future engine-side
/// `registerComponent` surfaces with no edit here beyond an optional hint.
///
/// Units (the 57x bug guard): ONLY `Transform.rotation` converts — UI shows degrees,
/// the wire carries radians — driven by the `convertRadians` hint. SpotLight
/// innerAngle/outerAngle are degrees on BOTH sides (no conversion); their `unit:"deg"`
/// is just a label/clamp. The widget value passed in/out of `renderField` is always
/// already in the WIRE unit (radians for rotation); conversion happens at the widget
/// boundary inside this file.
import { NumberDrag } from "./NumberDrag";
import { SliderField } from "./SliderField";
import { VectorEditor } from "./VectorEditor";
import { ColorField } from "./ColorField";
import { AssetPicker } from "./AssetPicker";
import { EnumField, type EnumOption } from "./EnumField";
import { LockAxesField } from "./LockAxesField";
import { Switch } from "@/components/ui/switch";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { humanizeFieldName } from "@/lib/humanize";
import { DEG_TO_RAD, RAD_TO_DEG } from "@/lib/utils";

export type FieldKind =
  | "vec3"
  | "vec4"
  | "color3"
  | "color4"
  | "number"
  | "slider"
  | "bool"
  | "text"
  | "uuid"
  | "enum"
  | "lockAxes"
  | "struct";

/// Asset kind a `uuid` field references (the AssetPicker filters the catalog by this).
export type AssetKind = "mesh" | "texture" | "material" | "model" | "animation";

export interface FieldHint {
  kind: FieldKind;
  min?: number;
  max?: number;
  step?: number;
  /// Degree semantics: `convertRadians` true converts UI<->wire (Transform.rotation
  /// only). `unit:"deg"` is a display label/clamp with NO conversion (spot angles).
  unit?: "deg";
  convertRadians?: boolean;
  /// For `uuid` fields: which asset catalog the picker filters to.
  asset?: AssetKind;
  /// For `enum` fields: the wire-string → Sentence-case-label option list.
  options?: readonly EnumOption[];
  /// For `enum` fields whose WIRE value is an integer (not a string): the option `value`s
  /// are the decimal index ("0"/"1"/…) and onChange emits a `number`. Used for the named
  /// collisionLayer Select over the fixed moving-slot mapping.
  numeric?: boolean;
  /// For `struct` fields: the per-sub-field hint keyed by sub-field name (nested
  /// objects like ColliderComponent.material → {friction, restitution}).
  fields?: Record<string, FieldHint>;
}

/// Per-field widget overrides for the built-in components. Keyed `Component.field`.
/// Anything not listed falls back to value-shape inference.
export const FIELD_HINTS: Record<string, FieldHint> = {
  "Name.name": { kind: "text" },

  "Transform.translation": { kind: "vec3", step: 0.05 },
  "Transform.scale": { kind: "vec3", step: 0.05 },
  // Edited in DEGREES, stored/serialized in RADIANS.
  "Transform.rotation": { kind: "vec3", step: 0.5, unit: "deg", convertRadians: true },

  "Mesh.mesh": { kind: "uuid", asset: "mesh" },
  "MaterialAsset.material": { kind: "uuid", asset: "material" },

  "Camera.fov": { kind: "number", min: 1, max: 179, step: 0.5 },
  "Camera.near": { kind: "number", min: 0.001, step: 0.01 },
  "Camera.far": { kind: "number", min: 0.1, step: 1 },
  "Camera.primary": { kind: "bool" },
  // Editor-gizmo overlay fields: the frustum/model glyphs and the frustum draw extent
  // (never negative — it's a world-space draw distance, not a clip plane).
  "Camera.showModel": { kind: "bool" },
  "Camera.showFrustum": { kind: "bool" },
  "Camera.frustumMaxDistance": { kind: "number", min: 0, step: 0.5 },

  "Material.baseColor": { kind: "color4" },
  "Material.albedoTexture": { kind: "uuid", asset: "texture" },
  "Material.metallicRoughnessTexture": { kind: "uuid", asset: "texture" },
  "Material.occlusionTexture": { kind: "uuid", asset: "texture" },
  "Material.normalTexture": { kind: "uuid", asset: "texture" },
  "Material.emissiveTexture": { kind: "uuid", asset: "texture" },
  "Material.heightTexture": { kind: "uuid", asset: "texture" },
  "Material.metallic": { kind: "slider", min: 0, max: 1, step: 0.01 },
  "Material.roughness": { kind: "slider", min: 0, max: 1, step: 0.01 },
  "Material.normalStrength": { kind: "number", min: 0, max: 4, step: 0.01 },
  "Material.heightScale": { kind: "number", min: 0, max: 0.5, step: 0.001 },
  "Material.emissive": { kind: "color3" },
  "Material.emissiveStrength": { kind: "number", min: 0, max: 100, step: 0.05 },
  "Material.blend": {
    kind: "enum",
    options: [
      { value: "opaque", label: "Opaque" },
      { value: "masked", label: "Masked" },
      { value: "translucent", label: "Translucent" },
    ],
  },
  "Material.alphaCutoff": { kind: "slider", min: 0, max: 1, step: 0.01 },
  "Material.unlit": { kind: "bool" },

  "DirectionalLight.direction": { kind: "vec3", step: 0.01 },
  "DirectionalLight.color": { kind: "color3" },
  "DirectionalLight.intensity": { kind: "number", min: 0, max: 50, step: 0.05 },
  "DirectionalLight.ambient": { kind: "slider", min: 0, max: 1, step: 0.01 },

  "PointLight.color": { kind: "color3" },
  "PointLight.intensity": { kind: "number", min: 0, max: 100, step: 0.05 },
  "PointLight.range": { kind: "number", min: 0, max: 200, step: 0.1 },

  "SpotLight.direction": { kind: "vec3", step: 0.01 },
  "SpotLight.color": { kind: "color3" },
  "SpotLight.intensity": { kind: "number", min: 0, max: 100, step: 0.05 },
  "SpotLight.range": { kind: "number", min: 0, max: 200, step: 0.1 },
  // Degrees on BOTH sides — unit:"deg" is label/clamp only, NO conversion.
  "SpotLight.innerAngle": { kind: "number", min: 0, max: 89, step: 0.1, unit: "deg" },
  "SpotLight.outerAngle": { kind: "number", min: 0, max: 89, step: 0.1, unit: "deg" },

  "ReflectionProbe.influenceRadius": { kind: "number", min: 0.1, max: 500, step: 0.1 },
  "ReflectionProbe.intensity": { kind: "slider", min: 0, max: 8, step: 0.01 },
  "ReflectionProbe.boxProjection": { kind: "bool" },
  "ReflectionProbe.boxExtent": { kind: "vec3", step: 0.1 },

  // Rigidbody — motion is solver-relevant only for Dynamic (documented; all fields rendered).
  "Rigidbody.motion": {
    kind: "enum",
    options: [
      { value: "static", label: "Static" },
      { value: "kinematic", label: "Kinematic" },
      { value: "dynamic", label: "Dynamic" },
    ],
  },
  "Rigidbody.mass": { kind: "number", min: 0, step: 0.1 },
  "Rigidbody.linearDamping": { kind: "slider", min: 0, max: 1, step: 0.01 },
  "Rigidbody.angularDamping": { kind: "slider", min: 0, max: 1, step: 0.01 },
  "Rigidbody.gravityFactor": { kind: "number", min: 0, max: 2, step: 0.05 },
  "Rigidbody.lockPosition": { kind: "lockAxes" },
  "Rigidbody.lockRotation": { kind: "lockAxes" },
  // The moving-slot the body lives in (resolveObjectLayer: 0=Moving, 1=Character, 2=Debris).
  // Static/Sensor derive from motion/isSensor, so they are not offered here. Wire value = the int.
  "Rigidbody.collisionLayer": {
    kind: "enum",
    numeric: true,
    options: [
      { value: "0", label: "Moving" },
      { value: "1", label: "Character" },
      { value: "2", label: "Debris" },
    ],
  },

  // Collider — material is nested; sourceMesh needs the explicit mesh hint (uuid default is texture).
  "Collider.shape": {
    kind: "enum",
    options: [
      { value: "box", label: "Box" },
      { value: "sphere", label: "Sphere" },
      { value: "capsule", label: "Capsule" },
      { value: "convexhull", label: "Convex hull" },
      { value: "mesh", label: "Mesh" },
    ],
  },
  "Collider.halfExtents": { kind: "vec3", min: 0, step: 0.05 },
  "Collider.sourceMesh": { kind: "uuid", asset: "mesh" },
  "Collider.offset": { kind: "vec3", step: 0.05 },
  "Collider.material": {
    kind: "struct",
    fields: {
      friction: { kind: "slider", min: 0, max: 1, step: 0.01 },
      restitution: { kind: "slider", min: 0, max: 1, step: 0.01 },
    },
  },
  "Collider.isSensor": { kind: "bool" },

  // CharacterController — maxSlopeAngle is radians on the wire → scalar convertRadians.
  "CharacterController.maxSpeed": { kind: "number", min: 0, step: 0.1 },
  "CharacterController.maxSlopeAngle": {
    kind: "number",
    min: 0,
    max: 89,
    step: 0.5,
    unit: "deg",
    convertRadians: true,
  },
  "CharacterController.maxStepHeight": { kind: "number", min: 0, step: 0.01 },
  "CharacterController.gravityFactor": { kind: "number", min: 0, max: 2, step: 0.05 },

  // KinematicBones — only `enabled` is meaningfully editable; `driven` (int[]) renders as a
  // read-only joint summary in the Inspector's structured body, not as a field here.
  "KinematicBones.enabled": { kind: "bool" },

  // ModelInstance — the `.smodel` this expanded subtree was instantiated from (set by import;
  // a model-asset reference shown through the model picker rather than as a raw id).
  "ModelInstance.modelId": { kind: "uuid", asset: "model" },

  // AnimationPlayer — `clip` is an animation-asset reference; `wrap`/`transitionMode` are closed
  // enums; `loopBlend` is a 0..1 wrap-blend (only meaningful for Loop). `time` is left to the
  // numeric fallback (runtime playhead; scrubbing belongs to the Timeline transport).
  "AnimationPlayer.clip": { kind: "uuid", asset: "animation" },
  "AnimationPlayer.autoplay": { kind: "bool" },
  "AnimationPlayer.speed": { kind: "number", min: 0, step: 0.05 },
  "AnimationPlayer.playing": { kind: "bool" },
  "AnimationPlayer.loopBlend": { kind: "slider", min: 0, max: 1, step: 0.01 },
  "AnimationPlayer.wrap": {
    kind: "enum",
    options: [
      { value: "once", label: "Once" },
      { value: "loop", label: "Loop" },
      { value: "pingpong", label: "Ping pong" },
    ],
  },
  "AnimationPlayer.transitionMode": {
    kind: "enum",
    options: [
      { value: "crossfade", label: "Cross fade" },
      { value: "inertialize", label: "Inertialize" },
    ],
  },

  // FootIk — the editable scalars in its structured body (the chains[] vector renders as a
  // read-only joint-chain summary there, not as a field).
  "FootIk.enabled": { kind: "bool" },
  "FootIk.groundHeight": { kind: "number", step: 0.01 },
};

function isVec3(v: unknown): v is Record<string, number> {
  return typeof v === "object" && v !== null && "x" in v && "y" in v && "z" in v && !("w" in v);
}

function isVec4(v: unknown): v is Record<string, number> {
  return typeof v === "object" && v !== null && "x" in v && "y" in v && "z" in v && "w" in v;
}

/// Infer a kind from the value shape when no FIELD_HINTS entry exists. Keeps an
/// unmapped (e.g. newly added) field renderable instead of dropped.
export function inferKind(value: unknown): FieldKind {
  if (isVec4(value)) {
    return "vec4";
  }
  if (isVec3(value)) {
    return "vec3";
  }
  if (typeof value === "number") {
    return "number";
  }
  if (typeof value === "boolean") {
    return "bool";
  }
  return "text";
}

export function resolveHint(component: string, field: string, value: unknown): FieldHint {
  const hint = FIELD_HINTS[`${component}.${field}`];
  if (hint) {
    return hint;
  }
  return { kind: inferKind(value) };
}

export interface FieldRenderContext {
  /// Drag bracket so the panel can gate the reconcile poll off mid-scrub.
  onDragStart(): void;
  onDragEnd(): void;
}

/// Render one field's widget. `value` is the raw wire value (rotation in radians).
/// `onChange(next)` receives the new WIRE value (rotation already converted back to
/// radians here), ready for the panel's read-modify-write. Resolves the hint, then
/// dispatches by kind through `renderByHint` (factored out so the `struct` kind can
/// recurse into its sub-fields without re-keying through `resolveHint`).
export function renderField(
  component: string,
  field: string,
  value: unknown,
  onChange: (next: unknown) => void,
  ctx: FieldRenderContext,
): React.ReactElement {
  return renderByHint(resolveHint(component, field, value), value, onChange, ctx);
}

/// Dispatch a widget purely from a resolved `FieldHint` (no component/field keying).
/// Called by `renderField` and recursively by the `struct` branch for each sub-field;
/// `ctx` is forwarded unchanged so a nested slider scrub still gates the reconcile poll.
function renderByHint(
  hint: FieldHint,
  value: unknown,
  onChange: (next: unknown) => void,
  ctx: FieldRenderContext,
): React.ReactElement {
  switch (hint.kind) {
    case "vec3":
    case "vec4": {
      const axes =
        hint.kind === "vec4" ? (["x", "y", "z", "w"] as const) : (["x", "y", "z"] as const);
      const wire = (value ?? {}) as Record<string, number>;
      // Display in degrees only for the converting hint (Transform.rotation).
      const display: Record<string, number> = hint.convertRadians
        ? Object.fromEntries(axes.map((a) => [a, (wire[a] ?? 0) * RAD_TO_DEG]))
        : wire;
      return (
        <VectorEditor
          axes={axes}
          value={display}
          step={hint.step}
          onChange={(patch) => {
            const wirePatch = hint.convertRadians
              ? Object.fromEntries(Object.entries(patch).map(([a, v]) => [a, v * DEG_TO_RAD]))
              : patch;
            onChange({ ...wire, ...wirePatch });
          }}
          onDragStart={ctx.onDragStart}
          onDragEnd={ctx.onDragEnd}
        />
      );
    }

    case "color3":
    case "color4": {
      const wire = (value ?? {}) as Record<string, number>;
      return (
        <ColorField
          kind={hint.kind}
          value={wire}
          onChange={(patch) => onChange({ ...wire, ...patch })}
          onDragStart={ctx.onDragStart}
          onDragEnd={ctx.onDragEnd}
        />
      );
    }

    case "slider":
      return (
        <SliderField
          value={typeof value === "number" ? value : 0}
          min={hint.min ?? 0}
          max={hint.max ?? 1}
          step={hint.step ?? 0.01}
          onChange={(v) => onChange(v)}
          onDragStart={ctx.onDragStart}
          onDragEnd={ctx.onDragEnd}
        />
      );

    case "number": {
      // Scalar degree conversion (UI degrees, wire radians) for the converting hint —
      // today CharacterController.maxSlopeAngle only. Guarded strictly on the hint so
      // every other number field is untouched.
      const raw = typeof value === "number" ? value : 0;
      const display = hint.convertRadians ? raw * RAD_TO_DEG : raw;
      return (
        <NumberDrag
          value={display}
          min={hint.min}
          max={hint.max}
          step={hint.step}
          onChange={(v) => onChange(hint.convertRadians ? v * DEG_TO_RAD : v)}
          onDragStart={ctx.onDragStart}
          onDragEnd={ctx.onDragEnd}
        />
      );
    }

    case "bool":
      return <Switch checked={value === true} onCheckedChange={(checked) => onChange(checked)} />;

    case "enum":
      return (
        <EnumField
          value={hint.numeric ? String(value ?? 0) : typeof value === "string" ? value : ""}
          options={hint.options ?? []}
          onChange={(v) => onChange(hint.numeric ? Number(v) : v)}
        />
      );

    case "lockAxes": {
      const wire = (value ?? {}) as Record<string, boolean>;
      return <LockAxesField value={wire} onChange={(patch) => onChange({ ...wire, ...patch })} />;
    }

    case "struct": {
      const wire = (value ?? {}) as Record<string, unknown>;
      const fields = hint.fields ?? {};
      return (
        <div className="flex flex-col gap-1.5 rounded-sm border border-border/60 bg-muted/20 px-2 py-1.5">
          {Object.entries(fields).map(([sub, subHint]) => (
            <div key={sub} className="grid grid-cols-[72px_1fr] items-center gap-1.5">
              <Label className="truncate text-[11px] font-normal text-muted-foreground">
                {humanizeFieldName(sub)}
              </Label>
              <div className="min-w-0">
                {renderByHint(
                  subHint,
                  wire[sub],
                  (next) => onChange({ ...wire, [sub]: next }),
                  ctx,
                )}
              </div>
            </div>
          ))}
        </div>
      );
    }

    case "uuid":
      // A Uuid field → the thumbnail combo + drag-drop target. The asset catalog
      // it filters to comes from the hint (`asset: "mesh" | "texture"`); a Uuid
      // field with no hint defaults to texture (the common case beyond Mesh.mesh).
      return (
        <AssetPicker
          value={typeof value === "string" ? value : "0"}
          assetType={hint.asset ?? "texture"}
          onChange={(v) => onChange(v)}
        />
      );

    case "text":
    default:
      return (
        <Input
          type="text"
          className="h-7 rounded-sm bg-background px-1.5 py-0.5 font-mono text-[11px]"
          value={typeof value === "string" ? value : JSON.stringify(value)}
          onFocus={ctx.onDragStart}
          onBlur={ctx.onDragEnd}
          onChange={(event) => onChange(event.currentTarget.value)}
        />
      );
  }
}
