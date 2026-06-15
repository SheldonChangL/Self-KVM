//! Injection backend built on `enigo` (cross-platform input simulation).
//!
//! Modifier state is *not* applied from the `mask`: the server forwards explicit
//! modifier key-down/up events, so the correct shifted character emerges from
//! replaying those presses — exactly how the reference protocol behaves.

use enigo::{
    Axis, Button, Coordinate, Direction, Enigo, Key, Keyboard, Mouse, Settings,
};
use kvm_core::{InputCommand, KeyAction};
use kvm_proto::keys::{self, key, KeyId};

use crate::{InputError, Injector};

pub struct EnigoInjector {
    enigo: Enigo,
}

impl EnigoInjector {
    pub fn new() -> Result<Self, InputError> {
        let enigo = Enigo::new(&Settings::default())
            .map_err(|e| InputError::Backend(format!("enigo init: {e}")))?;
        Ok(Self { enigo })
    }
}

impl Injector for EnigoInjector {
    fn inject(&mut self, cmd: InputCommand) -> Result<(), InputError> {
        let be = |e: enigo::InputError| InputError::Backend(format!("enigo: {e}"));
        match cmd {
            InputCommand::MouseMoveAbs { x, y } => {
                self.enigo.move_mouse(x, y, Coordinate::Abs).map_err(be)
            }
            InputCommand::MouseMoveRel { dx, dy } => {
                self.enigo.move_mouse(dx, dy, Coordinate::Rel).map_err(be)
            }
            InputCommand::MouseButton { button, pressed } => {
                let b = map_button(button);
                let d = if pressed {
                    Direction::Press
                } else {
                    Direction::Release
                };
                self.enigo.button(b, d).map_err(be)
            }
            InputCommand::MouseWheel { x, y } => {
                if y != 0 {
                    self.enigo.scroll(wheel_lines(y), Axis::Vertical).map_err(be)?;
                }
                if x != 0 {
                    self.enigo
                        .scroll(wheel_lines(x), Axis::Horizontal)
                        .map_err(be)?;
                }
                Ok(())
            }
            InputCommand::Key { id, action, .. } => {
                let d = match action {
                    KeyAction::Down | KeyAction::Repeat(_) => Direction::Press,
                    KeyAction::Up => Direction::Release,
                };
                match map_key(id) {
                    Some(k) => self.enigo.key(k, d).map_err(be),
                    None => {
                        tracing::debug!("unmapped key id {id:#06x}; skipping");
                        Ok(())
                    }
                }
            }
        }
    }
}

fn map_button(button: i8) -> Button {
    match button {
        1 => Button::Left,
        2 => Button::Middle,
        3 => Button::Right,
        4 => Button::Back,
        5 => Button::Forward,
        _ => Button::Left,
    }
}

/// Convert a protocol wheel delta (≈120 per notch) into enigo scroll lines.
/// enigo treats positive as "down"/"right", so we negate to match the usual
/// "positive delta scrolls up" convention.
fn wheel_lines(delta: i16) -> i32 {
    let d = delta as i32;
    let lines = d / 120;
    if lines == 0 {
        -d.signum()
    } else {
        -lines
    }
}

fn map_key(id: KeyId) -> Option<Key> {
    if keys::is_printable(id) {
        return char::from_u32(id as u32).map(Key::Unicode);
    }
    let k = match id {
        key::BACKSPACE => Key::Backspace,
        key::TAB => Key::Tab,
        key::RETURN => Key::Return,
        key::ESCAPE => Key::Escape,
        key::DELETE => Key::Delete,
        key::HOME => Key::Home,
        key::END => Key::End,
        key::PAGE_UP => Key::PageUp,
        key::PAGE_DOWN => Key::PageDown,
        key::LEFT => Key::LeftArrow,
        key::RIGHT => Key::RightArrow,
        key::UP => Key::UpArrow,
        key::DOWN => Key::DownArrow,
        key::SHIFT_L | key::SHIFT_R => Key::Shift,
        key::CONTROL_L | key::CONTROL_R => Key::Control,
        key::ALT_L | key::ALT_R => Key::Alt,
        key::SUPER_L | key::SUPER_R | key::META_L | key::META_R => Key::Meta,
        key::CAPS_LOCK => Key::CapsLock,
        f if (key::F1..=key::f(12)).contains(&f) => return f_key(f - key::F1 + 1),
        _ => return None,
    };
    Some(k)
}

fn f_key(n: KeyId) -> Option<Key> {
    Some(match n {
        1 => Key::F1,
        2 => Key::F2,
        3 => Key::F3,
        4 => Key::F4,
        5 => Key::F5,
        6 => Key::F6,
        7 => Key::F7,
        8 => Key::F8,
        9 => Key::F9,
        10 => Key::F10,
        11 => Key::F11,
        12 => Key::F12,
        _ => return None,
    })
}
