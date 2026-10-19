//! input.rs - turns æther events into egui's RawInput events.
//!
//! æther reports key presses only, as printable ASCII, '\n', '\b', '\t', 27
//! or KEY_*, plus MOD_* bits. Each press becomes an egui key press and
//! release (and a Text event for printable characters without Ctrl/Alt).
//! Mouse events carry the full button mask, so presses and releases of every
//! button are found by comparing it with the last one.

use aether_host::{abi, Event as Ev, Mouse};
use egui::{Event, Key, Modifiers, MouseWheelUnit, PointerButton, Pos2, TouchPhase, Vec2};

pub struct InputState {
    pub pointer: Option<Pos2>,
    buttons: u32,
    pub modifiers: Modifiers,
    pub focused: bool,
    /// In-process clipboard: what the app last copied.
    pub clipboard: String,
    pub close_requested: bool,
    pub resized: Option<(u32, u32)>,
    pub events: Vec<Event>,
}

fn modifiers(mods: u32) -> Modifiers {
    let ctrl = mods & abi::MOD_CTRL != 0;
    Modifiers {
        alt: mods & abi::MOD_ALT != 0,
        ctrl,
        shift: mods & abi::MOD_SHIFT != 0,
        mac_cmd: false,
        command: ctrl,
    }
}

/// egui's key for an æther key code, if it has one.
pub fn key_of(code: u32) -> Option<Key> {
    Some(match code {
        0x0A | 0x0D => Key::Enter,
        0x08 | 0x7F => Key::Backspace,
        0x09 => Key::Tab,
        27 => Key::Escape,
        abi::KEY_UP => Key::ArrowUp,
        abi::KEY_DOWN => Key::ArrowDown,
        abi::KEY_LEFT => Key::ArrowLeft,
        abi::KEY_RIGHT => Key::ArrowRight,
        abi::KEY_HOME => Key::Home,
        abi::KEY_END => Key::End,
        abi::KEY_PGUP => Key::PageUp,
        abi::KEY_PGDN => Key::PageDown,
        abi::KEY_DELETE => Key::Delete,
        0x20..=0x7E => {
            let c = char::from(code as u8);
            return Key::from_name(c.encode_utf8(&mut [0; 4]));
        }
        _ => return None,
    })
}

/// The key on a US keyboard that types `c` (for `physical_key`).
fn physical_of(key: Key) -> Option<Key> {
    let name = key.name();
    let is_plain = name.len() == 1 && name.as_bytes()[0].is_ascii_alphanumeric();
    is_plain.then_some(key)
}

impl InputState {
    pub fn new() -> Self {
        InputState {
            pointer: None,
            buttons: 0,
            modifiers: Modifiers::default(),
            focused: true,
            clipboard: String::new(),
            close_requested: false,
            resized: None,
            events: Vec::new(),
        }
    }

    /// Handles one backend event; window coordinates are pixels.
    pub fn on_event(&mut self, ev: Ev, pixels_per_point: f32) {
        match ev {
            Ev::Key { key, mods } => self.on_key(key, mods),
            Ev::MouseMove(m) | Ev::MouseDown(m) | Ev::MouseUp(m) => self.on_mouse(m, pixels_per_point),
            Ev::Scroll { dx, dy, mods } => {
                self.set_modifiers(mods);
                self.events.push(Event::MouseWheel {
                    unit: MouseWheelUnit::Line,
                    delta: Vec2::new(dx as f32, dy as f32),
                    phase: TouchPhase::Move,
                    modifiers: self.modifiers,
                });
            }
            Ev::Close => self.close_requested = true,
            Ev::Focus | Ev::Unfocus => {
                self.focused = ev == Ev::Focus;
                self.events.push(Event::WindowFocused(self.focused));
                if !self.focused {
                    // Buttons and modifiers released elsewhere never reach us.
                    self.release_all();
                }
            }
            Ev::Resize { width, height } => self.resized = Some((width, height)),
        }
    }

    fn set_modifiers(&mut self, mods: u32) {
        let m = modifiers(mods);
        if m != self.modifiers {
            self.modifiers = m;
            self.events.push(Event::ModifiersChanged(m));
        }
    }

    fn release_all(&mut self) {
        if let Some(pos) = self.pointer {
            for (bit, button) in BUTTONS {
                if self.buttons & bit != 0 {
                    self.events.push(Event::PointerButton { pos, button, pressed: false, modifiers: self.modifiers });
                }
            }
        }
        self.buttons = 0;
        self.set_modifiers(0);
    }

    fn on_key(&mut self, code: u32, mods: u32) {
        self.set_modifiers(mods);
        let m = self.modifiers;
        let key = key_of(code);
        if let Some(k) = key {
            // As egui-winit: the clipboard shortcuts become clipboard events.
            if m.command && !m.alt {
                match k {
                    Key::C => return self.events.push(Event::Copy),
                    Key::X => return self.events.push(Event::Cut),
                    Key::V => {
                        if !self.clipboard.is_empty() {
                            self.events.push(Event::Paste(self.clipboard.clone()));
                        } else {
                            // No text on the clipboard: still report the key,
                            // so an app with its own clipboard (Files) can act.
                            for pressed in [true, false] {
                                self.events.push(Event::Key {
                                    key: k,
                                    physical_key: physical_of(k),
                                    pressed,
                                    repeat: false,
                                    modifiers: m,
                                });
                            }
                        }
                        return;
                    }
                    _ => {}
                }
            }
            for pressed in [true, false] {
                self.events.push(Event::Key { key: k, physical_key: physical_of(k), pressed, repeat: false, modifiers: m });
            }
        }
        if (0x20..=0x7E).contains(&code) && !m.ctrl && !m.alt {
            self.events.push(Event::Text(char::from(code as u8).to_string()));
        }
    }

    fn on_mouse(&mut self, m: Mouse, ppp: f32) {
        self.set_modifiers(m.mods);
        let pos = Pos2::new(m.x as f32 / ppp, m.y as f32 / ppp);
        if self.pointer != Some(pos) {
            self.pointer = Some(pos);
            self.events.push(Event::PointerMoved(pos));
        }
        let changed = m.buttons ^ self.buttons;
        for (bit, button) in BUTTONS {
            if changed & bit != 0 {
                self.events.push(Event::PointerButton {
                    pos,
                    button,
                    pressed: m.buttons & bit != 0,
                    modifiers: self.modifiers,
                });
            }
        }
        self.buttons = m.buttons;
    }
}

const BUTTONS: [(u32, PointerButton); 3] =
    [(1, PointerButton::Primary), (2, PointerButton::Secondary), (4, PointerButton::Middle)];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_keys_and_text() {
        let mut s = InputState::new();
        s.on_event(Ev::Key { key: 'A' as u32, mods: abi::MOD_SHIFT }, 1.0);
        assert!(s.events.contains(&Event::Text("A".into())));
        assert!(s.events.iter().any(|e| matches!(e, Event::Key { key: Key::A, pressed: true, modifiers, .. } if modifiers.shift)));
        s.events.clear();
        s.on_event(Ev::Key { key: 'a' as u32, mods: abi::MOD_CTRL }, 1.0);
        assert!(!s.events.iter().any(|e| matches!(e, Event::Text(_))));
        s.on_event(Ev::Key { key: abi::KEY_LEFT, mods: 0 }, 1.0);
        assert!(s.events.iter().any(|e| matches!(e, Event::Key { key: Key::ArrowLeft, .. })));
        s.events.clear();
        s.clipboard = "hi".into();
        s.on_event(Ev::Key { key: 'v' as u32, mods: abi::MOD_CTRL }, 1.0);
        assert!(s.events.contains(&Event::Paste("hi".into())));
        // With no text clipboard, an app with its own clipboard still needs
        // to see Ctrl+V as a key.
        s.events.clear();
        s.clipboard.clear();
        s.on_event(Ev::Key { key: 'v' as u32, mods: abi::MOD_CTRL }, 1.0);
        assert!(s.events.iter().any(|e| matches!(e, Event::Key { key: Key::V, pressed: true, .. })));
    }

    #[test]
    fn finds_button_changes() {
        let mut s = InputState::new();
        let m = |buttons| Mouse { x: 4, y: 6, buttons, mods: 0 };
        s.on_event(Ev::MouseDown(m(1)), 2.0);
        s.on_event(Ev::MouseMove(m(3)), 2.0);
        s.on_event(Ev::MouseUp(m(0)), 2.0);
        let presses: Vec<_> = s
            .events
            .iter()
            .filter_map(|e| match e {
                Event::PointerButton { button, pressed, pos, .. } => Some((*button, *pressed, *pos)),
                _ => None,
            })
            .collect();
        let p = Pos2::new(2.0, 3.0);
        assert_eq!(
            presses,
            vec![
                (PointerButton::Primary, true, p),
                (PointerButton::Secondary, true, p),
                (PointerButton::Primary, false, p),
                (PointerButton::Secondary, false, p)
            ]
        );
    }
}
