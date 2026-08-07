//! The material height-map technique, shared by the asset model, the resolve, and the renderer.

/// How a material's height map is rendered.
///
/// The wire form (scene JSON, `.smat`) is the lowercase string [`HeightMode::as_wire`] returns;
/// [`HeightMode::from_wire`] parses it, mapping anything unknown to [`HeightMode::Bump`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum HeightMode {
    /// Height feeds the shading normal only: no parallax, no geometry, no artifacts. The
    /// far-field and low-poly degrade for the other modes.
    #[default]
    Bump,
    /// Parallax occlusion mapping: a fragment UV march fakes depth, leaving a flat silhouette.
    Parallax,
    /// Per-vertex displacement through the `displace` compute pre-pass into the shared deformed
    /// buffer — a true silhouette across every pass, BLAS-able, on a densely tessellated mesh.
    Displacement,
}

impl HeightMode {
    /// The lowercase wire token (`"bump"` / `"parallax"` / `"displacement"`) used in scene
    /// JSON and `.smat` documents.
    #[must_use]
    pub fn as_wire(self) -> &'static str {
        match self {
            HeightMode::Bump => "bump",
            HeightMode::Parallax => "parallax",
            HeightMode::Displacement => "displacement",
        }
    }

    /// Parses a wire token; anything unrecognized falls back to [`HeightMode::Bump`].
    #[must_use]
    pub fn from_wire(s: &str) -> Self {
        match s {
            "parallax" => HeightMode::Parallax,
            "displacement" => HeightMode::Displacement,
            _ => HeightMode::Bump,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn height_mode_round_trips_through_the_wire() {
        for mode in [
            HeightMode::Bump,
            HeightMode::Parallax,
            HeightMode::Displacement,
        ] {
            assert_eq!(HeightMode::from_wire(mode.as_wire()), mode);
        }
        assert_eq!(HeightMode::from_wire("nonsense"), HeightMode::Bump);
        assert_eq!(HeightMode::default(), HeightMode::Bump);
    }
}
