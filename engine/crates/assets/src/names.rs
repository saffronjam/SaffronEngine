//! The frozen wire strings for [`AssetType`] and [`Colorspace`].
//!
//! These name maps are a contract with the catalog cache, the `.smodel` META chunk,
//! and the editor: the strings are written into `catalog.json`, the META `subAsset`
//! records, and the `.smeta` sidecars, and read back by the scan. They live in one
//! module so the bake, the scan, and the container codec all spell them identically.

use saffron_scene::{AssetType, Colorspace, TextureRole};

/// The wire string for an [`AssetType`].
#[must_use]
pub fn asset_type_name(asset_type: AssetType) -> &'static str {
    match asset_type {
        AssetType::Texture => "texture",
        AssetType::Other => "other",
        AssetType::Animation => "animation",
        AssetType::Material => "material",
        AssetType::Model => "model",
        AssetType::Mesh => "mesh",
    }
}

/// The [`AssetType`] for a wire `type` string. An unknown string falls back to
/// [`AssetType::Mesh`].
#[must_use]
pub fn asset_type_from_name(name: &str) -> AssetType {
    match name {
        "texture" => AssetType::Texture,
        "other" => AssetType::Other,
        "animation" => AssetType::Animation,
        "material" => AssetType::Material,
        "model" => AssetType::Model,
        _ => AssetType::Mesh,
    }
}

/// The wire string for a [`Colorspace`].
#[must_use]
pub fn colorspace_name(space: Colorspace) -> &'static str {
    match space {
        Colorspace::Srgb => "srgb",
        Colorspace::Linear => "linear",
        Colorspace::Hdr => "hdr",
        Colorspace::Auto => "auto",
    }
}

/// The [`Colorspace`] for a wire string. An unknown string falls back to
/// [`Colorspace::Auto`].
#[must_use]
pub fn colorspace_from_name(name: &str) -> Colorspace {
    match name {
        "srgb" => Colorspace::Srgb,
        "linear" => Colorspace::Linear,
        "hdr" => Colorspace::Hdr,
        _ => Colorspace::Auto,
    }
}

/// The wire string for a [`TextureRole`] (the `.smeta` `role` field + the catalog cache).
#[must_use]
pub fn texture_role_name(role: TextureRole) -> &'static str {
    match role {
        TextureRole::Albedo => "albedo",
        TextureRole::Normal => "normal",
        TextureRole::Roughness => "roughness",
        TextureRole::Metallic => "metallic",
        TextureRole::Ao => "ao",
        TextureRole::Height => "height",
        TextureRole::Emissive => "emissive",
        TextureRole::Opacity => "opacity",
        TextureRole::Orm => "orm",
        TextureRole::Gloss => "gloss",
        TextureRole::Hdri => "hdri",
        TextureRole::Unknown => "unknown",
    }
}

/// The [`TextureRole`] for a canonical wire string. An unknown string falls back to
/// [`TextureRole::Unknown`]. This is the strict map for round-tripping a stored role; loose
/// filename/connector hints go through [`crate::infer_texture_role`] / [`crate::texture_role_from_hint`].
#[must_use]
pub fn texture_role_from_name(name: &str) -> TextureRole {
    match name {
        "albedo" => TextureRole::Albedo,
        "normal" => TextureRole::Normal,
        "roughness" => TextureRole::Roughness,
        "metallic" => TextureRole::Metallic,
        "ao" => TextureRole::Ao,
        "height" => TextureRole::Height,
        "emissive" => TextureRole::Emissive,
        "opacity" => TextureRole::Opacity,
        "orm" => TextureRole::Orm,
        "gloss" => TextureRole::Gloss,
        "hdri" => TextureRole::Hdri,
        _ => TextureRole::Unknown,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn asset_type_names_round_trip() {
        for ty in [
            AssetType::Texture,
            AssetType::Other,
            AssetType::Animation,
            AssetType::Material,
            AssetType::Model,
            AssetType::Mesh,
        ] {
            assert_eq!(asset_type_from_name(asset_type_name(ty)), ty);
        }
        // An unknown string is the Mesh default.
        assert_eq!(asset_type_from_name("nonsense"), AssetType::Mesh);
    }

    #[test]
    fn colorspace_names_round_trip() {
        for space in [
            Colorspace::Srgb,
            Colorspace::Linear,
            Colorspace::Hdr,
            Colorspace::Auto,
        ] {
            assert_eq!(colorspace_from_name(colorspace_name(space)), space);
        }
        // An unknown string is the Auto default.
        assert_eq!(colorspace_from_name("nonsense"), Colorspace::Auto);
    }

    #[test]
    fn texture_role_names_round_trip() {
        for role in [
            TextureRole::Albedo,
            TextureRole::Normal,
            TextureRole::Roughness,
            TextureRole::Metallic,
            TextureRole::Ao,
            TextureRole::Height,
            TextureRole::Emissive,
            TextureRole::Opacity,
            TextureRole::Orm,
            TextureRole::Gloss,
            TextureRole::Hdri,
            TextureRole::Unknown,
        ] {
            assert_eq!(texture_role_from_name(texture_role_name(role)), role);
        }
        // An unknown string is the Unknown default.
        assert_eq!(texture_role_from_name("nonsense"), TextureRole::Unknown);
    }
}
