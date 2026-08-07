//! Observability mirror for the reactive render loop.
//!
//! The loop's redraw verdict lives on `saffron_app::RedrawController`, above this crate in the DAG,
//! so the host pushes a per-frame snapshot down into the renderer and the editor's
//! window-visibility signal sets the [`PowerState`] back up. The control plane reports the snapshot
//! in `render-stats`, and the host reads the power state each frame to suppress rendering when the
//! viewport is hidden.

/// Whether the editor viewport is on-screen, so the host can throttle a hidden viewport.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum PowerState {
    /// The editor window has focus — render normally.
    #[default]
    Focused,
    /// The editor window is open but unfocused — the editor may slow its polling; the engine still
    /// renders on demand, but paces those frames down to [`UNFOCUSED_FPS_CAP`] to save power.
    Unfocused,
    /// The viewport is occluded or the window minimized — the host suppresses rendering entirely.
    Occluded,
}

impl PowerState {
    /// The wire / CLI name.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            PowerState::Focused => "focused",
            PowerState::Unfocused => "unfocused",
            PowerState::Occluded => "occluded",
        }
    }

    /// Parses a power-state name, `None` on an unknown value.
    #[must_use]
    pub fn from_name(name: &str) -> Option<PowerState> {
        match name {
            "focused" => Some(PowerState::Focused),
            "unfocused" => Some(PowerState::Unfocused),
            "occluded" => Some(PowerState::Occluded),
            _ => None,
        }
    }

    /// Whether the host should suppress rendering entirely in this state (the viewport is hidden).
    #[must_use]
    pub fn suppresses_render(self) -> bool {
        matches!(self, PowerState::Occluded)
    }

    /// The fps cap to apply to *rendered* frames in this state, or `None` for the full target.
    /// `Unfocused` paces continuous render down to [`UNFOCUSED_FPS_CAP`] so a backgrounded viewport
    /// stops pinning the GPU; `Focused` runs at the full target and `Occluded` renders nothing
    /// (suppressed separately, so it has no rate of its own).
    #[must_use]
    pub fn pace_fps_cap(self) -> Option<f64> {
        match self {
            PowerState::Unfocused => Some(UNFOCUSED_FPS_CAP),
            PowerState::Focused | PowerState::Occluded => None,
        }
    }
}

/// The fps an unfocused-but-visible editor viewport paces continuous render down to. Low enough to
/// idle the GPU while the user works elsewhere, high enough that an animating preview still updates
/// — the low single-digit background cap mature editors (Unreal, Unity) settle on.
pub const UNFOCUSED_FPS_CAP: f64 = 6.0;

/// The per-frame reactive-loop snapshot the host pushes into the renderer for `render-stats` to
/// report: whether the loop is idling, whether the temporal effects have converged, and the named
/// reasons continuous render is currently held.
#[derive(Clone, Debug, Default)]
pub struct ReactiveState {
    /// The loop is skipping renders (a static, converged viewport, or suppressed).
    pub idle: bool,
    /// The temporal effects (TAA / SSGI history) have settled to their final image.
    pub converged: bool,
    /// The reasons continuous render is currently held (empty when idle).
    pub reasons: Vec<String>,
    /// Whether the editor viewport is focused / unfocused / occluded.
    pub power_state: PowerState,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_unfocused_paces_down() {
        assert_eq!(PowerState::Focused.pace_fps_cap(), None);
        assert_eq!(PowerState::Occluded.pace_fps_cap(), None);
        assert_eq!(
            PowerState::Unfocused.pace_fps_cap(),
            Some(UNFOCUSED_FPS_CAP)
        );
    }

    #[test]
    fn occluded_suppresses_others_do_not() {
        assert!(PowerState::Occluded.suppresses_render());
        assert!(!PowerState::Focused.suppresses_render());
        assert!(!PowerState::Unfocused.suppresses_render());
    }
}
