//! Normalised input types shared between the capture layer, the state machines,
//! and the injection layer.

use kvm_proto::keys::{KeyButton, KeyId, KeyModifierMask};
use serde::{Deserialize, Serialize};

/// What happened to a key.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum KeyAction {
    Down,
    Up,
    /// Auto-repeat with a repeat count.
    Repeat(u16),
}

/// An input event observed on the *server's* machine (produced by a capture
/// backend). Absolute motion is used while the cursor is on the local screen;
/// relative motion is used while input is grabbed and the cursor lives on a
/// remote screen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LocalEvent {
    MotionAbs { x: i32, y: i32 },
    MotionRel { dx: i32, dy: i32 },
    Button { button: i8, pressed: bool },
    Key {
        id: KeyId,
        mask: KeyModifierMask,
        button: KeyButton,
        action: KeyAction,
    },
    Wheel { x: i16, y: i16 },
}

/// A concrete action for an injection backend to perform on the *client's*
/// machine.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InputCommand {
    MouseMoveAbs { x: i32, y: i32 },
    MouseMoveRel { dx: i32, dy: i32 },
    MouseButton { button: i8, pressed: bool },
    MouseWheel { x: i16, y: i16 },
    Key {
        id: KeyId,
        mask: KeyModifierMask,
        button: KeyButton,
        action: KeyAction,
    },
}
