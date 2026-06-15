//! Key encoding tables.
//!
//! The protocol separates three concepts, and getting this separation right is
//! what makes cross-platform key forwarding correct:
//!
//! * [`KeyId`] — the *virtual* key: what character/symbol the press produces.
//!   It changes with modifiers (`a` vs `A`). Printable keys use their Unicode
//!   code point; special keys use the X11 keysym minus `0x1000` (the `0xEFxx`
//!   block); media/system keys live in a private `0xE0xx` block.
//! * [`KeyButton`] — the *physical* key (scan-code-like, layout independent).
//!   A receiver MUST track held keys by `KeyButton`, because the `KeyId` at
//!   release time may differ from press time once a modifier changed.
//! * [`KeyModifierMask`] — a bitmask of modifiers active at the moment of the
//!   event, so a receiver can re-synthesise the same effective key.

/// Virtual key id (16-bit on the wire).
pub type KeyId = u16;
/// Physical key / scan code (16-bit on the wire).
pub type KeyButton = u16;
/// Modifier bitmask (16-bit on the wire).
pub type KeyModifierMask = u16;

pub const KEY_NONE: KeyId = 0x0000;

// --- Modifier mask bits -----------------------------------------------------
pub mod modifier {
    use super::KeyModifierMask;
    pub const SHIFT: KeyModifierMask = 0x0001;
    pub const CONTROL: KeyModifierMask = 0x0002;
    pub const ALT: KeyModifierMask = 0x0004;
    pub const META: KeyModifierMask = 0x0008;
    pub const SUPER: KeyModifierMask = 0x0010;
    pub const ALT_GR: KeyModifierMask = 0x0020;
    pub const LEVEL5LOCK: KeyModifierMask = 0x0040;
    pub const CAPS_LOCK: KeyModifierMask = 0x1000;
    pub const NUM_LOCK: KeyModifierMask = 0x2000;
    pub const SCROLL_LOCK: KeyModifierMask = 0x4000;
}

// --- Special key ids (X11 keysym - 0x1000) ----------------------------------
pub mod key {
    use super::KeyId;

    pub const BACKSPACE: KeyId = 0xEF08;
    pub const TAB: KeyId = 0xEF09;
    pub const RETURN: KeyId = 0xEF0D;
    pub const ESCAPE: KeyId = 0xEF1B;
    pub const DELETE: KeyId = 0xEFFF;
    pub const SPACE: KeyId = 0x0020; // printable

    pub const HOME: KeyId = 0xEF50;
    pub const LEFT: KeyId = 0xEF51;
    pub const UP: KeyId = 0xEF52;
    pub const RIGHT: KeyId = 0xEF53;
    pub const DOWN: KeyId = 0xEF54;
    pub const PAGE_UP: KeyId = 0xEF55;
    pub const PAGE_DOWN: KeyId = 0xEF56;
    pub const END: KeyId = 0xEF57;
    pub const INSERT: KeyId = 0xEF63;

    // Function keys F1..F12 are contiguous from 0xEFBE.
    pub const F1: KeyId = 0xEFBE;
    pub const fn f(n: u8) -> KeyId {
        // n is 1-based (F1 == 1)
        F1 + (n as KeyId) - 1
    }

    // Modifier keys (left/right variants).
    pub const SHIFT_L: KeyId = 0xEFE1;
    pub const SHIFT_R: KeyId = 0xEFE2;
    pub const CONTROL_L: KeyId = 0xEFE3;
    pub const CONTROL_R: KeyId = 0xEFE4;
    pub const CAPS_LOCK: KeyId = 0xEFE5;
    pub const ALT_L: KeyId = 0xEFE9;
    pub const ALT_R: KeyId = 0xEFEA;
    pub const SUPER_L: KeyId = 0xEFEB;
    pub const SUPER_R: KeyId = 0xEFEC;
    pub const META_L: KeyId = 0xEFE7;
    pub const META_R: KeyId = 0xEFE8;
}

/// Map a printable character to its [`KeyId`] (its Unicode code point, clamped
/// to the 16-bit wire field). Non-BMP characters cannot be represented as a
/// bare `KeyId` and return `None`.
pub fn key_id_from_char(c: char) -> Option<KeyId> {
    let cp = c as u32;
    if cp <= 0xFFFF {
        Some(cp as KeyId)
    } else {
        None
    }
}

/// True if a [`KeyId`] denotes a directly printable character rather than a
/// special key from the `0xEFxx` / `0xE0xx` blocks.
pub fn is_printable(id: KeyId) -> bool {
    !(0xE000..=0xEFFF).contains(&id) && id != KEY_NONE
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn char_roundtrip() {
        assert_eq!(key_id_from_char('A'), Some(0x41));
        assert_eq!(key_id_from_char('a'), Some(0x61));
        assert_eq!(key_id_from_char(' '), Some(0x20));
    }

    #[test]
    fn function_keys_are_contiguous() {
        assert_eq!(key::f(1), key::F1);
        assert_eq!(key::f(12), key::F1 + 11);
    }

    #[test]
    fn printable_classification() {
        assert!(is_printable(0x41)); // 'A'
        assert!(!is_printable(key::RETURN));
        assert!(!is_printable(KEY_NONE));
    }
}
