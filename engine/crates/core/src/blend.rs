//! The material alpha/blend mode (the glTF `alphaMode` axis), shared by the scene
//! components, the asset resolve, and the renderer.

/// How a material's alpha resolves at raster time (glTF `alphaMode`).
///
/// The wire form (scene JSON, `.smat`) is the lowercase string [`BlendMode::as_wire`]
/// returns; [`BlendMode::from_wire`] parses it (unknown → [`BlendMode::Opaque`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum BlendMode {
    /// Alpha is ignored; the surface is fully opaque (depth-prepass + opaque pass).
    #[default]
    Opaque,
    /// Alpha-tested cutout: a fragment whose alpha is below the cutoff is discarded.
    Masked,
    /// Alpha-blended translucency: drawn sorted, blended, without writing depth.
    Blend,
}

impl BlendMode {
    /// The lowercase wire token (`"opaque"` / `"masked"` / `"translucent"`) — the glTF
    /// `alphaMode` spelling used in scene JSON and `.smat` documents.
    #[must_use]
    pub fn as_wire(self) -> &'static str {
        match self {
            BlendMode::Opaque => "opaque",
            BlendMode::Masked => "masked",
            BlendMode::Blend => "translucent",
        }
    }

    /// Parses a wire token; anything unrecognized falls back to [`BlendMode::Opaque`].
    #[must_use]
    pub fn from_wire(s: &str) -> Self {
        match s {
            "masked" => BlendMode::Masked,
            "translucent" => BlendMode::Blend,
            _ => BlendMode::Opaque,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blend_mode_round_trips_through_the_wire() {
        for mode in [BlendMode::Opaque, BlendMode::Masked, BlendMode::Blend] {
            assert_eq!(BlendMode::from_wire(mode.as_wire()), mode);
        }
        assert_eq!(BlendMode::from_wire("nonsense"), BlendMode::Opaque);
        assert_eq!(BlendMode::default(), BlendMode::Opaque);
    }
}
