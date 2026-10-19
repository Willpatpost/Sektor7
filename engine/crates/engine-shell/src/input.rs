//! Input-intent state machine: SDL3 events in, intents out.
//!
//! Stage 1 represents input as **intent only** — WASD movement axes,
//! mouse-look deltas, key/button edges, focus transitions, resize, and
//! quit. There is no gameplay movement, collision, camera physics, or
//! character controller here (Stage 1 authorization, exclusions).
//!
//! The state machine is deliberately **event-driven and self-owned**: it
//! never queries SDL's internal keyboard or mouse device state. This is
//! what makes synthetic-event tests meaningful — pushing an event into the
//! pump dispatches through this exact path — while honoring the Stage 1
//! authorization's warning that event injection verifies dispatch, not
//! physical hardware input.
//!
//! Focus-loss discipline: on `WindowEvent::FocusLost` every held key is
//! cleared and unconsumed mouse deltas are discarded, so stuck keys and
//! stale look deltas cannot survive a focus change; fresh input is
//! required after focus is regained.

use std::collections::HashSet;

use sdl3::event::{Event, WindowEvent};
use sdl3::keyboard::Keycode;
use sdl3::mouse::MouseButton;

/// One poll's worth of input intents.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct InputSnapshot {
    /// WASD forward axis: +1 while W is held, -1 while S is held.
    pub move_forward: i8,
    /// WASD strafe axis: +1 while D is held, -1 while A is held.
    pub move_strafe: i8,
    /// Mouse-look delta accumulated since the previous snapshot. Collected
    /// only while relative mouse capture is on; discarded on focus loss so
    /// stale deltas never carry across frames.
    pub look_delta: (f32, f32),
    /// Keys that transitioned to pressed in this snapshot.
    pub keys_pressed: Vec<Keycode>,
    /// Keys that transitioned to released in this snapshot.
    pub keys_released: Vec<Keycode>,
    /// Key-repeat events observed in this snapshot. Repeats are counted,
    /// never reported as fresh presses.
    pub key_repeats: u32,
    /// Mouse buttons pressed in this snapshot.
    pub mouse_pressed: Vec<MouseButton>,
    /// Mouse buttons released in this snapshot.
    pub mouse_released: Vec<MouseButton>,
    /// Window resize reported in this snapshot.
    pub resized: Option<(u32, u32)>,
    /// Focus regained in this snapshot.
    pub focus_gained: bool,
    /// Focus lost in this snapshot (held keys were cleared).
    pub focus_lost: bool,
    /// A quit was requested (window close or SDL quit); sticky once set.
    pub quit_requested: bool,
}

/// Event-driven input state.
pub struct InputState {
    /// Held-key set — membership only, never iterated: snapshot ordering
    /// comes from the order-preserving event `Vec`s, so this unordered
    /// collection cannot reach determinism-sensitive output.
    keys_held: HashSet<Keycode>,
    relative_capture: bool,
    look_delta: (f32, f32),
    keys_pressed: Vec<Keycode>,
    keys_released: Vec<Keycode>,
    key_repeats: u32,
    mouse_pressed: Vec<MouseButton>,
    mouse_released: Vec<MouseButton>,
    resized: Option<(u32, u32)>,
    focus_gained: bool,
    focus_lost: bool,
    quit_requested: bool,
}

impl Default for InputState {
    fn default() -> Self {
        Self::new()
    }
}

impl InputState {
    pub fn new() -> Self {
        InputState {
            keys_held: HashSet::new(),
            relative_capture: false,
            look_delta: (0.0, 0.0),
            keys_pressed: Vec::new(),
            keys_released: Vec::new(),
            key_repeats: 0,
            mouse_pressed: Vec::new(),
            mouse_released: Vec::new(),
            resized: None,
            focus_gained: false,
            focus_lost: false,
            quit_requested: false,
        }
    }

    pub fn is_held(&self, key: Keycode) -> bool {
        self.keys_held.contains(&key)
    }

    /// Whether look deltas are currently collected (the shell mirrors this
    /// in SDL's window relative-mouse mode).
    pub fn relative_capture(&self) -> bool {
        self.relative_capture
    }

    pub fn set_relative_capture(&mut self, on: bool) {
        self.relative_capture = on;
    }

    pub fn quit_requested(&self) -> bool {
        self.quit_requested
    }

    /// Processes one SDL event into the accumulators.
    pub fn process(&mut self, event: &Event) {
        match event {
            Event::Quit { .. } => self.quit_requested = true,
            Event::Window { win_event, .. } => self.process_window(*win_event),
            Event::KeyDown {
                keycode, repeat, ..
            } => {
                if *repeat {
                    self.key_repeats += 1;
                } else if let Some(key) = keycode
                    && self.keys_held.insert(*key)
                {
                    self.keys_pressed.push(*key);
                }
            }
            Event::KeyUp { keycode, .. } => {
                if let Some(key) = keycode
                    && self.keys_held.remove(key)
                {
                    self.keys_released.push(*key);
                }
            }
            Event::MouseMotion { xrel, yrel, .. } => {
                if self.relative_capture {
                    self.look_delta.0 += *xrel;
                    self.look_delta.1 += *yrel;
                }
            }
            Event::MouseButtonDown { mouse_btn, .. } => self.mouse_pressed.push(*mouse_btn),
            Event::MouseButtonUp { mouse_btn, .. } => self.mouse_released.push(*mouse_btn),
            _ => {}
        }
    }

    fn process_window(&mut self, win_event: WindowEvent) {
        match win_event {
            WindowEvent::CloseRequested => self.quit_requested = true,
            WindowEvent::Resized(width, height) => {
                self.resized = Some((width.max(0) as u32, height.max(0) as u32));
            }
            WindowEvent::FocusGained => self.focus_gained = true,
            WindowEvent::FocusLost => {
                self.focus_lost = true;
                // Anti-stuck-key and stale-delta discipline: nothing held
                // before a focus loss survives it.
                self.keys_held.clear();
                self.look_delta = (0.0, 0.0);
            }
            _ => {}
        }
    }

    /// Drains the accumulators into a snapshot for this frame.
    pub fn poll(&mut self) -> InputSnapshot {
        let move_forward = i8::from(self.keys_held.contains(&Keycode::W))
            - i8::from(self.keys_held.contains(&Keycode::S));
        let move_strafe = i8::from(self.keys_held.contains(&Keycode::D))
            - i8::from(self.keys_held.contains(&Keycode::A));
        let snapshot = InputSnapshot {
            move_forward,
            move_strafe,
            look_delta: self.look_delta,
            keys_pressed: std::mem::take(&mut self.keys_pressed),
            keys_released: std::mem::take(&mut self.keys_released),
            key_repeats: self.key_repeats,
            mouse_pressed: std::mem::take(&mut self.mouse_pressed),
            mouse_released: std::mem::take(&mut self.mouse_released),
            resized: self.resized,
            focus_gained: self.focus_gained,
            focus_lost: self.focus_lost,
            quit_requested: self.quit_requested,
        };
        self.look_delta = (0.0, 0.0);
        self.key_repeats = 0;
        self.resized = None;
        self.focus_gained = false;
        self.focus_lost = false;
        snapshot
    }
}
