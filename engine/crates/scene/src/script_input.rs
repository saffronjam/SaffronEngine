//! The gameplay-input snapshot scripts read: raw held keys + mouse, the derived per-tick
//! key/button edges, and the pointer deltas.
//!
//! It lives here because both `saffron-script` and `saffron-sceneedit` import Scene, so the shared
//! snapshot needs no cross-crate edge. The host forwards raw input over the script-input command,
//! then calls [`derive_script_input_edges`] once per tick — before the script tick — to compute the
//! edges against the previous tick and roll the memory forward.

use std::collections::HashSet;

/// The gameplay-input snapshot scripts read each tick.
///
/// `held` / `mouse_buttons` / `mouse_x` / `mouse_y` / `scroll` are the raw state the host
/// fills from the script-input command. The `pressed` / `released` / `mouse_pressed` /
/// `mouse_released` edge sets and the `mouse_dx` / `mouse_dy` deltas are derived by
/// [`derive_script_input_edges`]; the `prev_*` fields are its memory.
#[derive(Clone, Debug, Default)]
pub struct ScriptInputState {
    /// Keys down this tick (raw, from the script-input command).
    pub held: HashSet<String>,
    /// Mouse buttons down this tick: `"left"` / `"right"` / `"middle"` (raw).
    pub mouse_buttons: HashSet<String>,
    /// Viewport-relative pointer X (raw).
    pub mouse_x: f32,
    /// Viewport-relative pointer Y (raw).
    pub mouse_y: f32,
    /// Accumulated scroll (raw).
    pub scroll: f32,
    /// Keys that went down this tick, up last tick (derived).
    pub pressed: HashSet<String>,
    /// Keys that went up this tick, down last tick (derived).
    pub released: HashSet<String>,
    /// Buttons that went down this tick, up last tick (derived).
    pub mouse_pressed: HashSet<String>,
    /// Buttons that went up this tick, down last tick (derived).
    pub mouse_released: HashSet<String>,
    /// The per-tick pointer X delta (derived).
    pub mouse_dx: f32,
    /// The per-tick pointer Y delta (derived).
    pub mouse_dy: f32,
    /// The previous tick's held keys (derivation memory).
    pub prev_held: HashSet<String>,
    /// The previous tick's mouse buttons (derivation memory).
    pub prev_mouse_buttons: HashSet<String>,
    /// The previous tick's pointer X (derivation memory).
    pub prev_mouse_x: f32,
    /// The previous tick's pointer Y (derivation memory).
    pub prev_mouse_y: f32,
}

/// Derives the key/button edges and the pointer delta from this tick's raw held/mouse
/// state against the previous tick, then rolls the memory forward.
///
/// Called once per tick by the host before the script tick. A key in `held` but not
/// `prev_held` is a press this tick; one in `prev_held` but not `held` is a release;
/// likewise for the mouse buttons. The pointer delta is the raw position minus the
/// previous tick's. After deriving, the raw state becomes the next tick's memory.
pub fn derive_script_input_edges(input: &mut ScriptInputState) {
    input.pressed.clear();
    input.released.clear();
    for key in &input.held {
        if !input.prev_held.contains(key) {
            input.pressed.insert(key.clone());
        }
    }
    for key in &input.prev_held {
        if !input.held.contains(key) {
            input.released.insert(key.clone());
        }
    }

    input.mouse_pressed.clear();
    input.mouse_released.clear();
    for button in &input.mouse_buttons {
        if !input.prev_mouse_buttons.contains(button) {
            input.mouse_pressed.insert(button.clone());
        }
    }
    for button in &input.prev_mouse_buttons {
        if !input.mouse_buttons.contains(button) {
            input.mouse_released.insert(button.clone());
        }
    }

    input.mouse_dx = input.mouse_x - input.prev_mouse_x;
    input.mouse_dy = input.mouse_y - input.prev_mouse_y;

    input.prev_held = input.held.clone();
    input.prev_mouse_buttons = input.mouse_buttons.clone();
    input.prev_mouse_x = input.mouse_x;
    input.prev_mouse_y = input.mouse_y;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set(items: &[&str]) -> HashSet<String> {
        items.iter().map(|s| (*s).to_string()).collect()
    }

    #[test]
    fn first_tick_treats_all_held_as_pressed() {
        let mut input = ScriptInputState {
            held: set(&["w", "a"]),
            mouse_buttons: set(&["left"]),
            mouse_x: 10.0,
            mouse_y: 20.0,
            ..ScriptInputState::default()
        };

        derive_script_input_edges(&mut input);

        assert_eq!(input.pressed, set(&["w", "a"]));
        assert!(input.released.is_empty());
        assert_eq!(input.mouse_pressed, set(&["left"]));
        assert!(input.mouse_released.is_empty());
        assert_eq!(input.mouse_dx, 10.0);
        assert_eq!(input.mouse_dy, 20.0);
        assert_eq!(input.prev_held, set(&["w", "a"]));
        assert_eq!(input.prev_mouse_x, 10.0);
        assert_eq!(input.prev_mouse_y, 20.0);
    }

    #[test]
    fn edges_and_delta_compute_against_prev_tick_then_roll_forward() {
        // Tick 1: w + left go down.
        let mut input = ScriptInputState {
            held: set(&["w"]),
            mouse_buttons: set(&["left"]),
            mouse_x: 5.0,
            mouse_y: 5.0,
            ..ScriptInputState::default()
        };
        derive_script_input_edges(&mut input);
        assert_eq!(input.pressed, set(&["w"]));

        // Tick 2: w stays down, d goes down, left releases, the pointer moves +3/-1.
        input.held = set(&["w", "d"]);
        input.mouse_buttons = HashSet::new();
        input.mouse_x = 8.0;
        input.mouse_y = 4.0;
        derive_script_input_edges(&mut input);

        // d is the only new press (w was already held); left released.
        assert_eq!(input.pressed, set(&["d"]));
        assert!(input.released.is_empty());
        assert!(input.mouse_pressed.is_empty());
        assert_eq!(input.mouse_released, set(&["left"]));
        assert_eq!(input.mouse_dx, 3.0);
        assert_eq!(input.mouse_dy, -1.0);

        // Tick 3: w releases, d stays.
        input.held = set(&["d"]);
        derive_script_input_edges(&mut input);
        assert!(input.pressed.is_empty());
        assert_eq!(input.released, set(&["w"]));
    }
}
