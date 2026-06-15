//! Capture backend built on `rdev`'s `grab` API — the one cross-platform Rust
//! path that can *suppress* an event (return `None`) as well as observe it,
//! which is what lets the server stop local input from reaching this machine
//! while the cursor is on a remote screen.
//!
//! Platform notes: macOS needs Accessibility permission and runs a CFRunLoop on
//! the capture thread; Linux uses evdev (needs `input` group) and works under
//! both X11 and Wayland; Windows uses low-level hooks.

use std::sync::{Arc, Mutex};
use std::thread;

use kvm_core::{KeyAction, LocalEvent};
use kvm_proto::keys::{key, KeyButton, KeyId};
use rdev::{grab, Button as RButton, Event, EventType, Key as RKey};

use crate::{EventSink, GrabSwitch, InputCapture, InputError};

pub struct RdevCapture;

impl RdevCapture {
    pub fn new() -> Self {
        Self
    }
}

impl InputCapture for RdevCapture {
    fn start(&self, sink: EventSink, grab_sw: GrabSwitch) -> Result<(), InputError> {
        // rdev's grab is a blocking call running its own run-loop, so it gets a
        // dedicated OS thread. Last cursor position is tracked with interior
        // mutability because the grab callback must be `Fn`, not `FnMut`.
        let last_pos: Arc<Mutex<Option<(f64, f64)>>> = Arc::new(Mutex::new(None));

        thread::Builder::new()
            .name("kvm-capture".into())
            .spawn(move || {
                let callback = move |event: Event| -> Option<Event> {
                    let grabbing = grab_sw.enabled();
                    translate(&event, grabbing, &last_pos, &sink);
                    // While grabbing we swallow every event so the local machine
                    // does not also react; otherwise it passes through.
                    if grabbing {
                        None
                    } else {
                        Some(event)
                    }
                };
                if let Err(e) = grab(callback) {
                    tracing::error!("rdev grab failed: {e:?} (check Accessibility / input-group permissions)");
                }
            })
            .map_err(|e| InputError::Backend(format!("spawn capture thread: {e}")))?;
        Ok(())
    }
}

fn translate(
    event: &Event,
    grabbing: bool,
    last_pos: &Arc<Mutex<Option<(f64, f64)>>>,
    sink: &EventSink,
) {
    match event.event_type {
        EventType::MouseMove { x, y } => {
            let mut lp = last_pos.lock().unwrap();
            let delta = lp.map(|(px, py)| ((x - px) as i32, (y - py) as i32));
            *lp = Some((x, y));
            if grabbing {
                if let Some((dx, dy)) = delta {
                    if dx != 0 || dy != 0 {
                        sink(LocalEvent::MotionRel { dx, dy });
                    }
                }
            } else {
                sink(LocalEvent::MotionAbs {
                    x: x as i32,
                    y: y as i32,
                });
            }
        }
        EventType::ButtonPress(b) => sink(LocalEvent::Button {
            button: map_button(b),
            pressed: true,
        }),
        EventType::ButtonRelease(b) => sink(LocalEvent::Button {
            button: map_button(b),
            pressed: false,
        }),
        EventType::Wheel { delta_x, delta_y } => sink(LocalEvent::Wheel {
            x: delta_x as i16,
            y: delta_y as i16,
        }),
        EventType::KeyPress(k) => {
            if let Some(id) = map_key(k) {
                sink(LocalEvent::Key {
                    id,
                    mask: 0,
                    button: id as KeyButton,
                    action: KeyAction::Down,
                });
            }
        }
        EventType::KeyRelease(k) => {
            if let Some(id) = map_key(k) {
                sink(LocalEvent::Key {
                    id,
                    mask: 0,
                    button: id as KeyButton,
                    action: KeyAction::Up,
                });
            }
        }
    }
}

fn map_button(b: RButton) -> i8 {
    match b {
        RButton::Left => 1,
        RButton::Middle => 2,
        RButton::Right => 3,
        RButton::Unknown(n) => n as i8,
    }
}

/// Map an rdev physical key to a protocol [`KeyId`]. Letters/digits use their
/// base (unshifted) code point; the shift state arrives as separate modifier
/// key events, so the receiver reconstructs the right character.
fn map_key(k: RKey) -> Option<KeyId> {
    use RKey::*;
    let id: KeyId = match k {
        // Letters
        KeyA => b'a' as KeyId,
        KeyB => b'b' as KeyId,
        KeyC => b'c' as KeyId,
        KeyD => b'd' as KeyId,
        KeyE => b'e' as KeyId,
        KeyF => b'f' as KeyId,
        KeyG => b'g' as KeyId,
        KeyH => b'h' as KeyId,
        KeyI => b'i' as KeyId,
        KeyJ => b'j' as KeyId,
        KeyK => b'k' as KeyId,
        KeyL => b'l' as KeyId,
        KeyM => b'm' as KeyId,
        KeyN => b'n' as KeyId,
        KeyO => b'o' as KeyId,
        KeyP => b'p' as KeyId,
        KeyQ => b'q' as KeyId,
        KeyR => b'r' as KeyId,
        KeyS => b's' as KeyId,
        KeyT => b't' as KeyId,
        KeyU => b'u' as KeyId,
        KeyV => b'v' as KeyId,
        KeyW => b'w' as KeyId,
        KeyX => b'x' as KeyId,
        KeyY => b'y' as KeyId,
        KeyZ => b'z' as KeyId,
        // Digits
        Num0 => b'0' as KeyId,
        Num1 => b'1' as KeyId,
        Num2 => b'2' as KeyId,
        Num3 => b'3' as KeyId,
        Num4 => b'4' as KeyId,
        Num5 => b'5' as KeyId,
        Num6 => b'6' as KeyId,
        Num7 => b'7' as KeyId,
        Num8 => b'8' as KeyId,
        Num9 => b'9' as KeyId,
        // Whitespace / editing
        Space => b' ' as KeyId,
        Return => key::RETURN,
        Tab => key::TAB,
        Backspace => key::BACKSPACE,
        Escape => key::ESCAPE,
        Delete => key::DELETE,
        Insert => key::INSERT,
        Home => key::HOME,
        End => key::END,
        PageUp => key::PAGE_UP,
        PageDown => key::PAGE_DOWN,
        // Arrows
        UpArrow => key::UP,
        DownArrow => key::DOWN,
        LeftArrow => key::LEFT,
        RightArrow => key::RIGHT,
        // Modifiers
        ShiftLeft => key::SHIFT_L,
        ShiftRight => key::SHIFT_R,
        ControlLeft => key::CONTROL_L,
        ControlRight => key::CONTROL_R,
        Alt => key::ALT_L,
        AltGr => key::ALT_R,
        MetaLeft => key::SUPER_L,
        MetaRight => key::SUPER_R,
        CapsLock => key::CAPS_LOCK,
        // Function keys
        F1 => key::f(1),
        F2 => key::f(2),
        F3 => key::f(3),
        F4 => key::f(4),
        F5 => key::f(5),
        F6 => key::f(6),
        F7 => key::f(7),
        F8 => key::f(8),
        F9 => key::f(9),
        F10 => key::f(10),
        F11 => key::f(11),
        F12 => key::f(12),
        // Common punctuation (US layout base glyphs)
        Minus => b'-' as KeyId,
        Equal => b'=' as KeyId,
        LeftBracket => b'[' as KeyId,
        RightBracket => b']' as KeyId,
        SemiColon => b';' as KeyId,
        Quote => b'\'' as KeyId,
        BackSlash => b'\\' as KeyId,
        Comma => b',' as KeyId,
        Dot => b'.' as KeyId,
        Slash => b'/' as KeyId,
        BackQuote => b'`' as KeyId,
        _ => return None,
    };
    Some(id)
}
