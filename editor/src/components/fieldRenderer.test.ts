import { describe, expect, test } from "bun:test";
import { FIELD_HINTS, type FieldHint, inferKind, resolveHint } from "./fieldRenderer";

describe("inferKind (value-shape fallback)", () => {
  test("a {x,y,z,w} object infers vec4", () => {
    expect(inferKind({ x: 1, y: 2, z: 3, w: 4 })).toBe("vec4");
  });

  test("a {x,y,z} object (no w) infers vec3", () => {
    expect(inferKind({ x: 1, y: 2, z: 3 })).toBe("vec3");
  });

  test("the presence of w decides vec4 over vec3 even when w is 0", () => {
    expect(inferKind({ x: 0, y: 0, z: 0, w: 0 })).toBe("vec4");
  });

  test("a number infers number", () => {
    expect(inferKind(42)).toBe("number");
    expect(inferKind(0)).toBe("number");
    expect(inferKind(-3.14)).toBe("number");
  });

  test("a boolean infers bool", () => {
    expect(inferKind(true)).toBe("bool");
    expect(inferKind(false)).toBe("bool");
  });

  test("a string falls back to text", () => {
    expect(inferKind("hello")).toBe("text");
    expect(inferKind("")).toBe("text");
  });

  test("null falls back to text (typeof null is object but isVec* guard rejects it)", () => {
    expect(inferKind(null)).toBe("text");
  });

  test("undefined falls back to text", () => {
    expect(inferKind(undefined)).toBe("text");
  });

  test("a partial vector missing an axis is not a vec3 → text", () => {
    expect(inferKind({ x: 1, y: 2 })).toBe("text");
  });

  test("an arbitrary object with no x/y/z falls back to text", () => {
    expect(inferKind({ foo: 1, bar: 2 })).toBe("text");
  });

  test("an empty object falls back to text", () => {
    expect(inferKind({})).toBe("text");
  });

  test("an array is an object but lacks x/y/z → text", () => {
    expect(inferKind([1, 2, 3])).toBe("text");
  });

  test("NaN is still a number kind (typeof NaN is number)", () => {
    expect(inferKind(Number.NaN)).toBe("number");
  });
});

describe("resolveHint (explicit table wins, else shape inference)", () => {
  test("returns the exact FIELD_HINTS entry for a known component.field", () => {
    expect(resolveHint("Camera", "fov", 60)).toEqual(FIELD_HINTS["Camera.fov"]);
    expect(resolveHint("Material", "metallic", 0.5)).toEqual({
      kind: "slider",
      min: 0,
      max: 1,
      step: 0.01,
    });
  });

  test("the explicit hint ignores the value entirely (a numeric Camera.fov hint wins over any shape)", () => {
    // Even handed a vec3-shaped value, the table entry for Camera.fov is returned verbatim.
    expect(resolveHint("Camera", "fov", { x: 1, y: 2, z: 3 })).toEqual(FIELD_HINTS["Camera.fov"]);
  });

  test("a uuid asset hint is returned with its asset catalog", () => {
    expect(resolveHint("Mesh", "mesh", "0")).toEqual({ kind: "uuid", asset: "mesh" });
    expect(resolveHint("Material", "albedoTexture", "0")).toEqual({
      kind: "uuid",
      asset: "texture",
    });
  });

  test("an unknown component.field falls back to inferKind on the value", () => {
    expect(resolveHint("Unknown", "vecField", { x: 1, y: 2, z: 3 })).toEqual({ kind: "vec3" });
    expect(resolveHint("Unknown", "vec4Field", { x: 1, y: 2, z: 3, w: 4 })).toEqual({
      kind: "vec4",
    });
    expect(resolveHint("Unknown", "num", 7)).toEqual({ kind: "number" });
    expect(resolveHint("Unknown", "flag", true)).toEqual({ kind: "bool" });
    expect(resolveHint("Unknown", "label", "x")).toEqual({ kind: "text" });
  });

  test("the key is the literal `component.field` join — a wrong component does not match", () => {
    // "Camera.fov" exists, but "NotCamera.fov" must fall through to inference.
    expect(resolveHint("NotCamera", "fov", 60)).toEqual({ kind: "number" });
  });

  test("a known field on the wrong component does not borrow another component's hint", () => {
    // Material.metallic is a slider; PointLight.metallic is not in the table → number by value.
    expect(resolveHint("PointLight", "metallic", 0.5)).toEqual({ kind: "number" });
  });
});

describe("the 57x radians-bug guard", () => {
  // The bug class: a Transform.rotation value carried on the wire in RADIANS (small
  // magnitudes ~0..6.28) must NOT be re-graded by anything that looks at the number's
  // size. Kind resolution keys ONLY off component.field, and shape inference keys ONLY
  // off object structure — never numeric magnitude — so a radians-looking value keeps
  // the converting hint.

  test("Transform.rotation always resolves to the converting vec3 hint", () => {
    const hint = resolveHint("Transform", "rotation", { x: 0, y: 0, z: 0 });
    expect(hint.kind).toBe("vec3");
    expect(hint.convertRadians).toBe(true);
    expect(hint.unit).toBe("deg");
  });

  test("a radians-magnitude rotation value still picks the converting hint (not mis-graded)", () => {
    // 0.7853 rad ≈ 45°. The value looks small; resolution must not down-grade it.
    const small = resolveHint("Transform", "rotation", { x: 0.7853, y: 1.5708, z: 0 });
    expect(small).toEqual(FIELD_HINTS["Transform.rotation"]);
    // A near-2π value (looks like ~360° if naively read) must resolve identically.
    const wrap = resolveHint("Transform", "rotation", { x: 6.2831, y: 0, z: 0 });
    expect(wrap).toEqual(FIELD_HINTS["Transform.rotation"]);
  });

  test("Transform.translation and Transform.scale are plain vec3 with NO conversion", () => {
    const t = resolveHint("Transform", "translation", { x: 1, y: 2, z: 3 });
    expect(t.kind).toBe("vec3");
    expect(t.convertRadians).toBeUndefined();
    expect(t.unit).toBeUndefined();
    const s = resolveHint("Transform", "scale", { x: 1, y: 1, z: 1 });
    expect(s.convertRadians).toBeUndefined();
  });

  test("SpotLight angles are degrees on BOTH sides — unit:deg label/clamp but NO conversion", () => {
    const inner = resolveHint("SpotLight", "innerAngle", 20);
    expect(inner.unit).toBe("deg");
    expect(inner.convertRadians).toBeUndefined();
    const outer = resolveHint("SpotLight", "outerAngle", 45);
    expect(outer.unit).toBe("deg");
    expect(outer.convertRadians).toBeUndefined();
  });

  test("convertRadians is set on every radians-backed degree field", () => {
    const converting = Object.entries(FIELD_HINTS).filter(([, h]) => h.convertRadians === true);
    expect(converting.map(([k]) => k).sort()).toEqual([
      "CharacterController.maxSlopeAngle",
      "Transform.rotation",
    ]);
  });

  test("degree-native fields use the label without radians conversion", () => {
    const degLabelOnly = Object.entries(FIELD_HINTS).filter(
      ([, h]) => h.unit === "deg" && h.convertRadians !== true,
    );
    // SpotLight inner/outer angle are the deg-label-only fields.
    expect(degLabelOnly.map(([k]) => k).sort()).toEqual([
      "SpotLight.innerAngle",
      "SpotLight.outerAngle",
    ]);
    for (const [, h] of degLabelOnly) {
      expect(h.convertRadians).toBeUndefined();
    }
  });
});

describe("FIELD_HINTS parity table sanity", () => {
  test("every hint declares a kind", () => {
    for (const [key, hint] of Object.entries(FIELD_HINTS)) {
      expect(hint.kind, `${key} should declare a kind`).toBeTruthy();
    }
  });

  test("every uuid hint declares an asset catalog", () => {
    const uuids = Object.entries(FIELD_HINTS).filter(([, h]) => h.kind === "uuid");
    expect(uuids.length).toBeGreaterThan(0);
    for (const [key, hint] of uuids) {
      expect(
        ["mesh", "texture", "material", "model", "animation", "vegetation-map"],
        `${key} asset`,
      ).toContain(hint.asset as string);
    }
  });

  test("slider hints declare a min and max range", () => {
    const sliders = Object.entries(FIELD_HINTS).filter(([, h]) => h.kind === "slider");
    for (const [key, hint] of sliders as [string, FieldHint][]) {
      expect(typeof hint.min, `${key} min`).toBe("number");
      expect(typeof hint.max, `${key} max`).toBe("number");
    }
  });
});
