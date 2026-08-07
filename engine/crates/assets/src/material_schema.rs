//! The exposed-parameter schema of a material: the one declared list of tweakable
//! parameters — each a `{ name, kind, default }` — that overrides validate against, the
//! inspector renders, and the params buffer is kept in step with.
//!
//! The fixed `mesh` übershader exposes the canonical PBR set in [`pbr_exposed_parameters`].
//! A graph-authored material exposes the same set: the graph adds a surface program, not a
//! different override vocabulary, so `baseColor` / `metallic` / … stay the tweakable
//! parameters. This single list is the source of truth for [`apply_overrides`] coverage
//! (see the drift test), override validation ([`ExposedParamKind::accepts`]), and the inspector's
//! override editor.
//!
//! [`apply_overrides`]: crate::apply_overrides

use serde_json::json;

use saffron_json::Value;

/// The type of an exposed material parameter — drives override validation and the
/// inspector widget.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExposedParamKind {
    /// A single `f32`.
    Scalar,
    /// A linear RGB colour (`[r, g, b]`).
    Color3,
    /// A linear RGBA colour (`[r, g, b, a]`).
    Color4,
    /// A two-component vector (`[x, y]`).
    Vec2,
    /// A boolean.
    Bool,
    /// The blend-mode enum: `opaque` | `masked` | `translucent`.
    Blend,
    /// A texture asset id — a decimal string or an unsigned number (`0` = none).
    Texture,
}

impl ExposedParamKind {
    /// The wire token naming the kind (for the `material-schema` DTO and the CLI).
    #[must_use]
    pub fn as_wire(self) -> &'static str {
        match self {
            ExposedParamKind::Scalar => "scalar",
            ExposedParamKind::Color3 => "color3",
            ExposedParamKind::Color4 => "color4",
            ExposedParamKind::Vec2 => "vec2",
            ExposedParamKind::Bool => "bool",
            ExposedParamKind::Blend => "blend",
            ExposedParamKind::Texture => "texture",
        }
    }

    /// Whether `value` is a well-typed value for this kind — the override-validation gate.
    #[must_use]
    pub fn accepts(self, value: &Value) -> bool {
        match self {
            ExposedParamKind::Scalar => value.as_f64().is_some(),
            ExposedParamKind::Color3 => is_number_array(value, 3),
            ExposedParamKind::Color4 => is_number_array(value, 4),
            ExposedParamKind::Vec2 => is_number_array(value, 2),
            ExposedParamKind::Bool => value.is_boolean(),
            ExposedParamKind::Blend => {
                matches!(value.as_str(), Some("opaque" | "masked" | "translucent"))
            }
            ExposedParamKind::Texture => value.is_string() || value.is_number(),
        }
    }
}

/// Whether `value` is an array of exactly `n` numbers.
fn is_number_array(value: &Value, n: usize) -> bool {
    value
        .as_array()
        .is_some_and(|a| a.len() == n && a.iter().all(|e| e.as_f64().is_some()))
}

/// One exposed parameter: its override key, type, and default value.
#[derive(Clone, Debug, PartialEq)]
pub struct ExposedParam {
    /// The override key — the `overrides` map key and the wire field name.
    pub name: &'static str,
    /// The parameter type.
    pub kind: ExposedParamKind,
    /// The übershader default — the value in force when no override applies.
    pub default: Value,
}

/// The canonical exposed-parameter list of the fixed PBR übershader — the schema every
/// material exposes for overrides.
///
/// Defaults mirror [`MaterialAsset::default`](crate::MaterialAsset). Texture defaults are
/// the `"0"` decimal-string sentinel, matching the `.smat` wire shape.
#[must_use]
pub fn pbr_exposed_parameters() -> Vec<ExposedParam> {
    use ExposedParamKind::{Blend, Bool, Color3, Color4, Scalar, Texture, Vec2};
    vec![
        param("baseColor", Color4, json!([1.0, 1.0, 1.0, 1.0])),
        param("metallic", Scalar, json!(0.0)),
        param("roughness", Scalar, json!(1.0)),
        param("emissive", Color3, json!([0.0, 0.0, 0.0])),
        param("emissiveStrength", Scalar, json!(1.0)),
        param("normalStrength", Scalar, json!(1.0)),
        param("alphaCutoff", Scalar, json!(0.5)),
        param("heightScale", Scalar, json!(0.05)),
        param("uvTiling", Vec2, json!([1.0, 1.0])),
        param("uvOffset", Vec2, json!([0.0, 0.0])),
        param("unlit", Bool, json!(false)),
        param("doubleSided", Bool, json!(false)),
        param("blend", Blend, json!("opaque")),
        param("albedoTexture", Texture, json!("0")),
        param("ormTexture", Texture, json!("0")),
        param("normalTexture", Texture, json!("0")),
        param("emissiveTexture", Texture, json!("0")),
        param("heightTexture", Texture, json!("0")),
    ]
}

/// Builds one [`ExposedParam`] — a terse constructor for [`pbr_exposed_parameters`].
fn param(name: &'static str, kind: ExposedParamKind, default: Value) -> ExposedParam {
    ExposedParam {
        name,
        kind,
        default,
    }
}

/// Looks up an exposed parameter by its override key, or `None` if the key is not a
/// tweakable parameter.
#[must_use]
pub fn exposed_parameter(name: &str) -> Option<ExposedParam> {
    pbr_exposed_parameters()
        .into_iter()
        .find(|p| p.name == name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_matches_each_kind() {
        assert!(ExposedParamKind::Scalar.accepts(&json!(0.5)));
        assert!(!ExposedParamKind::Scalar.accepts(&json!("nope")));
        assert!(ExposedParamKind::Color4.accepts(&json!([1, 2, 3, 4])));
        assert!(!ExposedParamKind::Color4.accepts(&json!([1, 2, 3])));
        assert!(ExposedParamKind::Color3.accepts(&json!([0, 0, 0])));
        assert!(ExposedParamKind::Vec2.accepts(&json!([1, 1])));
        assert!(ExposedParamKind::Bool.accepts(&json!(true)));
        assert!(!ExposedParamKind::Bool.accepts(&json!(1)));
        assert!(ExposedParamKind::Blend.accepts(&json!("masked")));
        assert!(!ExposedParamKind::Blend.accepts(&json!("glassy")));
        // A texture id may be a decimal string or an unsigned number.
        assert!(ExposedParamKind::Texture.accepts(&json!("42")));
        assert!(ExposedParamKind::Texture.accepts(&json!(42)));
        assert!(!ExposedParamKind::Texture.accepts(&json!(true)));
    }

    #[test]
    fn every_default_is_well_typed_for_its_kind() {
        for p in pbr_exposed_parameters() {
            assert!(
                p.kind.accepts(&p.default),
                "default for {} is not a valid {:?}",
                p.name,
                p.kind
            );
        }
    }

    #[test]
    fn lookup_finds_and_misses() {
        assert_eq!(
            exposed_parameter("metallic").unwrap().kind,
            ExposedParamKind::Scalar
        );
        assert!(exposed_parameter("bogus").is_none());
    }

    /// The drift guard: every exposed parameter must be handled by `apply_overrides`, so
    /// applying a valid override for each one changes the material away from its default.
    /// If a parameter is added to the schema but not to `apply_overrides`, this fails.
    #[test]
    fn apply_overrides_covers_every_exposed_parameter() {
        use crate::{MaterialAsset, apply_overrides};

        for p in pbr_exposed_parameters() {
            let value = distinct_value(p.kind);
            assert!(
                p.kind.accepts(&value),
                "test value for {} is ill-typed",
                p.name
            );
            let mut material = MaterialAsset::default();
            apply_overrides(&mut material, &json!({ p.name: value }));
            assert_ne!(
                material,
                MaterialAsset::default(),
                "override '{}' had no effect — apply_overrides is missing it",
                p.name
            );
        }
    }

    /// A well-typed value for `kind` that differs from every schema default of that kind.
    fn distinct_value(kind: ExposedParamKind) -> Value {
        match kind {
            ExposedParamKind::Scalar => json!(0.321),
            ExposedParamKind::Color3 => json!([0.5, 0.25, 0.75]),
            ExposedParamKind::Color4 => json!([0.5, 0.25, 0.75, 0.5]),
            ExposedParamKind::Vec2 => json!([3.0, 4.0]),
            ExposedParamKind::Bool => json!(true),
            ExposedParamKind::Blend => json!("masked"),
            ExposedParamKind::Texture => json!("42"),
        }
    }
}
