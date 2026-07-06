//! The material height-map technique — how a grayscale height map is realized, shared by
//! the asset model, the resolve, and the renderer so there is one vocabulary for it.

/// How a material's height map is rendered.
///
/// One grayscale Height Map slot; the *mode* selects the technique (the shape the major
/// engines converge on — Unity HDRP's `Displacement Mode`, Godot's Height feature, Blender's
/// Bump/Displacement modes). The wire form (scene JSON, `.smat`) is the lowercase string
/// [`HeightMode::as_wire`] returns; [`HeightMode::from_wire`] parses it (unknown →
/// [`HeightMode::Bump`], the artifact-free baseline).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum HeightMode {
    /// Height → shading-normal bump only: no parallax, no geometry. The safe, artifact-free
    /// baseline (Blender `Bump Only`); the far-field / low-poly degrade for the other modes.
    #[default]
    Bump,
    /// Parallax occlusion mapping: a fragment UV march fakes depth. Flat silhouette; the
    /// right tool for genuine height/parallax maps on near-perpendicular surfaces.
    Parallax,
    /// Real per-vertex displacement (the `displace` compute pre-pass moves geometry into the
    /// shared deformed buffer — true silhouette, consistent across every pass, BLAS-able).
    /// Needs a densely-tessellated mesh to show its silhouette.
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
        // Unknown tokens are bump; the default is bump.
        assert_eq!(HeightMode::from_wire("nonsense"), HeightMode::Bump);
        assert_eq!(HeightMode::default(), HeightMode::Bump);
    }
}
