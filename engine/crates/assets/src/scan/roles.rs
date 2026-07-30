//! Texture-role and height-technique inference from filenames.

use saffron_core::HeightMode;
use saffron_scene::{Colorspace, TextureRole};

use crate::names::texture_role_from_name;

/// Detects a texture's material-map role from its filename (lowercased substring match).
/// Returns an empty string when no role token is recognized.
#[must_use]
pub fn detect_material_role(filename: &str) -> &'static str {
    let lower = filename.to_ascii_lowercase();
    let has = |token: &str| lower.contains(token);
    // Normal is matched before the packed `arm`/`orm`/`mra` acronyms: `orm` is a substring of
    // `normal` (n-orm-al), so a bare-substring `orm` check would otherwise claim every normal
    // map (e.g. ambientCG's `NormalGL`). A real packed map never contains `normal`.
    if has("normal") || has("_nor") || has("nrm") {
        "normal"
    } else if has("arm") || has("orm") || has("_mra") {
        "orm"
    } else if has("albedo")
        || has("basecolor")
        || has("base_color")
        || has("diffuse")
        || has("_diff")
        || has("_col")
        || has("color")
    {
        "albedo"
    } else if has("rough") {
        "roughness"
    } else if has("metal") {
        "metallic"
    } else if has("emissive") || has("emission") || has("_emit") {
        "emissive"
    } else if has("height") || has("displace") || has("_disp") || has("bump") {
        "height"
    } else if has("occlusion") || has("_ao") || has("ambientocclusion") {
        "ao"
    } else if has("gloss") {
        "gloss"
    } else if has("opacity") || has("alpha") || has("_mask") {
        "opacity"
    } else {
        ""
    }
}

/// The height technique an imported map's filename implies (the material-import routing): an explicit
/// **bump** map → the shading-only [`HeightMode::Bump`]; every other height map → [`HeightMode::Parallax`]
/// (parallax-occlusion mapping). An imported map is **never** auto-routed to
/// [`HeightMode::Displacement`]: real tessellating displacement costs a per-frame amplification + BLAS
/// build, so it is a deliberate authored choice through the editor's material `heightMode` dropdown, not
/// an import inference — the flat / low-poly common case never silently pays for it. Only meaningful for
/// a map whose role is `height`; a per-material `heightMode` in the editor overrides it.
#[must_use]
pub fn detect_height_mode(filename: &str) -> HeightMode {
    let lower = filename.to_ascii_lowercase();
    if lower.contains("bump") {
        HeightMode::Bump
    } else {
        HeightMode::Parallax
    }
}

/// The [`TextureRole`] a texture should carry, inferred from its filename and HDR-ness.
/// An `.hdr`/float texture is always [`TextureRole::Hdri`]; otherwise the filename token
/// ([`detect_material_role`]) decides. An unrecognized name is [`TextureRole::Unknown`].
#[must_use]
pub fn infer_texture_role(name: &str, hdr: bool) -> TextureRole {
    if hdr {
        return TextureRole::Hdri;
    }
    texture_role_from_name(detect_material_role(name))
}

/// The [`TextureRole`] for a free-form hint — a connector's map role (`"color"`, `"nor_gl"`,
/// `"arm"`) or a filename fragment. Tries the strict canonical map first (so a connector that
/// already speaks canonical, e.g. `"normal"`, is exact and dodges the `detect_material_role`
/// `"normal" ⊃ "orm"` quirk), then falls back to the loose filename tokenizer.
#[must_use]
pub fn texture_role_from_hint(hint: &str) -> TextureRole {
    let lower = hint.to_ascii_lowercase();
    if lower == "hdri" || lower == "hdr" || lower == "environment" || lower == "equirect" {
        return TextureRole::Hdri;
    }
    let canonical = texture_role_from_name(&lower);
    if canonical != TextureRole::Unknown {
        return canonical;
    }
    texture_role_from_name(detect_material_role(&lower))
}

/// The upload [`Colorspace`] for a texture whose role was **explicitly** supplied (an import
/// connector or an `import-texture` `role` hint). Color/emissive → sRGB, HDRI → float, and every
/// other role — *including* [`TextureRole::Unknown`] — → linear, because an explicit role string
/// means a data map (a connector's `"specular"` / `"data"`), never a color image. Contrast the
/// scan mint of a *foreign* file, where an unrecognized filename stays sRGB.
#[must_use]
pub fn colorspace_for_role_explicit(role: TextureRole) -> Colorspace {
    match role {
        TextureRole::Hdri => Colorspace::Hdr,
        TextureRole::Albedo | TextureRole::Emissive => Colorspace::Srgb,
        _ => Colorspace::Linear,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detect_material_role_classifies_filenames() {
        assert_eq!(detect_material_role("rock_ARM.png"), "orm");
        assert_eq!(detect_material_role("wood_orm.jpg"), "orm");
        assert_eq!(detect_material_role("brick_BaseColor.png"), "albedo");
        assert_eq!(detect_material_role("metal_diffuse.tga"), "albedo");
        // Normal is matched before the packed `orm`/`arm`/`mra` acronyms, so a literal "normal"
        // name (which contains the substring "orm") classifies as "normal", as do the `_nor`/`nrm`
        // tokens. This is what lets ambientCG's `*_NormalGL` land in the normal slot.
        assert_eq!(detect_material_role("stone_nor.png"), "normal");
        assert_eq!(detect_material_role("floor_nrm.png"), "normal");
        assert_eq!(detect_material_role("literal_normal.png"), "normal");
        assert_eq!(
            detect_material_role("Rock063_2K-PNG_NormalGL.png"),
            "normal"
        );
        assert_eq!(detect_material_role("surface_roughness.png"), "roughness");
        assert_eq!(detect_material_role("plate_metallic.png"), "metallic");
        assert_eq!(detect_material_role("lava_emissive.png"), "emissive");
        assert_eq!(detect_material_role("wall_height.png"), "height");
        assert_eq!(detect_material_role("crate_AO.png"), "ao");
        assert_eq!(detect_material_role("shiny_gloss.png"), "gloss");
        assert_eq!(detect_material_role("glass_opacity.png"), "opacity");
        assert_eq!(detect_material_role("random_texture.png"), "");
    }

    /// The height-map technique routing: an imported map is **never** auto-routed to Displacement (that
    /// costs tessellation + a per-frame BLAS, so it is a deliberate editor choice); an explicit bump map →
    /// shading bump, every other height map → parallax.
    #[test]
    fn detect_height_mode_never_auto_routes_displacement() {
        use saffron_core::HeightMode;
        // A provider Displacement map imports as Parallax — the user promotes it in the editor's dropdown.
        assert_eq!(
            detect_height_mode("Rock063_2K-PNG_Displacement.png"),
            HeightMode::Parallax
        );
        assert_eq!(detect_height_mode("wood_disp.png"), HeightMode::Parallax);
        assert_eq!(detect_height_mode("brick_bump.png"), HeightMode::Bump);
        assert_eq!(detect_height_mode("stone_height.png"), HeightMode::Parallax);
    }
}
