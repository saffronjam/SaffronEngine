//! The render-quality tier: one named knob that expands to the per-effect parameters of the
//! scalable screen-space GI stack (SSGI, GTAO, contact shadows).
//!
//! A [`QualityTier`] resolves to a [`RenderQuality`] the renderer applies to [`crate::Ssao`]; the
//! `Custom` tier carries hand-tuned parameters. Every parameter it dials is already a runtime
//! push-constant, so applying a tier costs nothing.

/// A named render-quality level. Higher tiers spend more GPU on the screen-space GI stack.
///
/// `Custom` is the escape hatch: the accompanying [`RenderQuality`] carries hand-set parameters
/// instead of a tier preset. The wire encoding is the kebab-case [`QualityTier::as_str`] /
/// [`QualityTier::from_name`] pair (the control DTO and project file both use the string name).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum QualityTier {
    /// Direct + image-based lighting only — the screen-space GI stack is off. Cheapest.
    Low,
    /// SSGI + GTAO at reduced step counts, contact shadows off. A good interactive default.
    Medium,
    /// The full screen-space stack at standard step counts (the engine's historical look).
    High,
    /// The full stack at elevated step counts, for stills / screenshots / strong hardware.
    Ultra,
    /// Parameters set by hand rather than a preset.
    Custom,
}

impl QualityTier {
    /// All preset tiers in ascending cost order (excludes `Custom`), for editor menus / CLI help.
    pub const PRESETS: [QualityTier; 4] = [
        QualityTier::Low,
        QualityTier::Medium,
        QualityTier::High,
        QualityTier::Ultra,
    ];

    /// The wire / CLI name.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            QualityTier::Low => "low",
            QualityTier::Medium => "medium",
            QualityTier::High => "high",
            QualityTier::Ultra => "ultra",
            QualityTier::Custom => "custom",
        }
    }

    /// Parses a tier name (kebab-case), `None` on an unknown value.
    #[must_use]
    pub fn from_name(name: &str) -> Option<QualityTier> {
        match name {
            "low" => Some(QualityTier::Low),
            "medium" => Some(QualityTier::Medium),
            "high" => Some(QualityTier::High),
            "ultra" => Some(QualityTier::Ultra),
            "custom" => Some(QualityTier::Custom),
            _ => None,
        }
    }

    /// Expands a preset tier to its [`RenderQuality`] parameters. `Custom` resolves to the `High`
    /// parameters as a sane base a caller then overrides field-by-field.
    #[must_use]
    pub fn resolve(self) -> RenderQuality {
        match self {
            QualityTier::Low => RenderQuality {
                tier: self,
                ssgi_enabled: false,
                ssgi_steps: 4.0,
                ssgi_rays: 4.0,
                gtao_enabled: false,
                contact_enabled: false,
                contact_steps: 8.0,
            },
            QualityTier::Medium => RenderQuality {
                tier: self,
                ssgi_enabled: true,
                ssgi_steps: 4.0,
                ssgi_rays: 3.0,
                gtao_enabled: true,
                contact_enabled: false,
                contact_steps: 8.0,
            },
            QualityTier::High | QualityTier::Custom => RenderQuality {
                tier: self,
                ssgi_enabled: true,
                ssgi_steps: 8.0,
                ssgi_rays: 4.0,
                gtao_enabled: true,
                contact_enabled: true,
                contact_steps: 12.0,
            },
            QualityTier::Ultra => RenderQuality {
                tier: self,
                ssgi_enabled: true,
                ssgi_steps: 12.0,
                ssgi_rays: 6.0,
                gtao_enabled: true,
                contact_enabled: true,
                contact_steps: 16.0,
            },
        }
    }
}

/// The resolved per-effect parameters of the scalable screen-space GI stack.
///
/// The renderer applies this to [`crate::Ssao`] (the enable flags + the SSGI / contact step counts
/// that are runtime push-constants). `tier` records which preset produced it (or `Custom`).
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct RenderQuality {
    /// The tier this came from (`Custom` if hand-set).
    pub tier: QualityTier,
    /// Screen-space one-bounce GI on.
    pub ssgi_enabled: bool,
    /// SSGI ray-march steps (the `ssgi.slang` push `params.z`).
    pub ssgi_steps: f32,
    /// SSGI cosine-hemisphere rays per pixel (the `ssgi.slang` push `params2.x`).
    pub ssgi_rays: f32,
    /// GTAO ambient occlusion on.
    pub gtao_enabled: bool,
    /// Screen-space contact shadows on.
    pub contact_enabled: bool,
    /// Contact-shadow ray-march steps (the `contact.slang` push `params.y`).
    pub contact_steps: f32,
}

impl Default for RenderQuality {
    /// The engine's historical look: the full screen-space stack at standard step counts (`High`).
    fn default() -> Self {
        QualityTier::High.resolve()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tiers_round_trip_through_their_names() {
        for tier in [
            QualityTier::Low,
            QualityTier::Medium,
            QualityTier::High,
            QualityTier::Ultra,
            QualityTier::Custom,
        ] {
            assert_eq!(QualityTier::from_name(tier.as_str()), Some(tier));
        }
        assert_eq!(QualityTier::from_name("nonsense"), None);
    }

    #[test]
    fn cost_rises_monotonically_with_the_tier() {
        let low = QualityTier::Low.resolve();
        let medium = QualityTier::Medium.resolve();
        let high = QualityTier::High.resolve();
        let ultra = QualityTier::Ultra.resolve();
        // Low disables the screen-space stack; each step up enables more or marches further.
        assert!(!low.ssgi_enabled && !low.gtao_enabled && !low.contact_enabled);
        assert!(medium.ssgi_enabled && medium.gtao_enabled && !medium.contact_enabled);
        assert!(high.contact_enabled);
        assert!(ultra.ssgi_steps > high.ssgi_steps);
        assert!(high.ssgi_steps > medium.ssgi_steps);
        // Ray count scales the same way: more cosine-hemisphere samples at the higher tiers.
        assert!(ultra.ssgi_rays > high.ssgi_rays);
        assert!(high.ssgi_rays >= medium.ssgi_rays);
    }

    #[test]
    fn default_is_the_high_historical_look() {
        assert_eq!(RenderQuality::default().tier, QualityTier::High);
    }
}
