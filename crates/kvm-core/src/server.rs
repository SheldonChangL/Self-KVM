//! The server-side routing brain.
//!
//! [`ServerMachine`] is a pure state machine: feed it normalised [`LocalEvent`]s
//! captured on the server's machine and it returns the [`ServerAction`]s the
//! runtime must carry out — forwarding protocol messages to a client, toggling
//! the local input grab, or warping the local cursor when control returns home.
//! Holding no I/O makes the whole switching logic exhaustively testable.

use kvm_proto::keys::{modifier, KeyModifierMask};
use kvm_proto::Message;

use crate::events::{KeyAction, LocalEvent};
use crate::layout::ScreenLayout;

/// Something the runtime must do as a result of an input event.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ServerAction {
    /// Send `msg` to the client owning `screen`.
    Send { screen: String, msg: Message },
    /// Start (`true`) or stop (`false`) grabbing/suppressing local input.
    SetGrab(bool),
    /// Warp the local OS cursor to `(x, y)` on the local screen.
    WarpCursor { x: i32, y: i32 },
    /// The active screen changed (for status/UI).
    ActiveChanged { screen: String },
}

pub struct ServerMachine {
    layout: ScreenLayout,
    local_screen: String,
    active: String,
    cursor_x: i32,
    cursor_y: i32,
    modifiers: KeyModifierMask,
    seq: u32,
    /// When true, the cursor is pinned to the active screen and edge crossings
    /// are ignored (Barrier's "lock to screen" hotkey).
    switch_locked: bool,
}

impl ServerMachine {
    /// Create a machine whose cursor starts at the centre of the local screen.
    pub fn new(layout: ScreenLayout, local_screen: impl Into<String>) -> Self {
        let local_screen = local_screen.into();
        let (cx, cy) = layout
            .size_of(&local_screen)
            .map(|s| (s.w / 2, s.h / 2))
            .unwrap_or((0, 0));
        Self {
            layout,
            active: local_screen.clone(),
            local_screen,
            cursor_x: cx,
            cursor_y: cy,
            modifiers: 0,
            seq: 0,
            switch_locked: false,
        }
    }

    pub fn is_locked(&self) -> bool {
        self.switch_locked
    }

    pub fn set_lock(&mut self, locked: bool) {
        self.switch_locked = locked;
    }

    /// Toggle the screen lock; returns the new state.
    pub fn toggle_lock(&mut self) -> bool {
        self.switch_locked = !self.switch_locked;
        self.switch_locked
    }

    pub fn active_screen(&self) -> &str {
        &self.active
    }

    /// True while the cursor is on a remote screen (i.e. local input is grabbed
    /// and forwarded rather than acted on locally).
    pub fn is_remote(&self) -> bool {
        self.active != self.local_screen
    }

    pub fn layout(&self) -> &ScreenLayout {
        &self.layout
    }

    /// Replace the layout at runtime (e.g. the user edited it in the GUI). Resets
    /// control to the local screen to avoid dangling on a screen that vanished.
    pub fn set_layout(&mut self, layout: ScreenLayout) {
        self.layout = layout;
        self.active = self.local_screen.clone();
    }

    /// Force control back to the local screen, releasing any grab. Used when the
    /// active client disconnects so the operator is never stranded on a dead
    /// screen. Returns the actions needed to settle that transition.
    pub fn go_home(&mut self) -> Vec<ServerAction> {
        if !self.is_remote() {
            return Vec::new();
        }
        self.active = self.local_screen.clone();
        let (cx, cy) = self
            .layout
            .size_of(&self.local_screen)
            .map(|s| (s.w / 2, s.h / 2))
            .unwrap_or((0, 0));
        self.cursor_x = cx;
        self.cursor_y = cy;
        vec![
            ServerAction::SetGrab(false),
            ServerAction::WarpCursor { x: cx, y: cy },
            ServerAction::ActiveChanged {
                screen: self.local_screen.clone(),
            },
        ]
    }

    /// Drive the machine with one captured event.
    pub fn handle(&mut self, ev: LocalEvent) -> Vec<ServerAction> {
        self.track_modifiers(&ev);
        if self.is_remote() {
            self.handle_remote(ev)
        } else {
            self.handle_local(ev)
        }
    }

    // --- cursor on the local (primary) screen ------------------------------
    fn handle_local(&mut self, ev: LocalEvent) -> Vec<ServerAction> {
        match ev {
            LocalEvent::MotionAbs { x, y } => {
                self.cursor_x = x;
                self.cursor_y = y;
                self.try_cross()
            }
            // While local we also accept relative motion (some backends only
            // produce deltas); accumulate then test for a crossing.
            LocalEvent::MotionRel { dx, dy } => {
                self.cursor_x += dx;
                self.cursor_y += dy;
                self.try_cross()
            }
            // Buttons / keys / wheel on the local screen are left for the OS to
            // handle — we are not grabbing, so nothing to forward.
            _ => Vec::new(),
        }
    }

    // --- cursor on a remote (client) screen --------------------------------
    fn handle_remote(&mut self, ev: LocalEvent) -> Vec<ServerAction> {
        match ev {
            LocalEvent::MotionRel { dx, dy } => {
                self.cursor_x += dx;
                self.cursor_y += dy;
                self.try_cross()
            }
            LocalEvent::MotionAbs { .. } => {
                // Ignore absolute motion while grabbed; we track a virtual cursor
                // from deltas. (A stray abs event can occur right after grab.)
                Vec::new()
            }
            LocalEvent::Button { button, pressed } => {
                let msg = if pressed {
                    Message::MouseDown { button }
                } else {
                    Message::MouseUp { button }
                };
                self.send_active(msg)
            }
            LocalEvent::Wheel { x, y } => self.send_active(Message::MouseWheel { x, y }),
            LocalEvent::Key {
                id,
                mask,
                button,
                action,
            } => {
                let msg = match action {
                    KeyAction::Down => Message::KeyDown { id, mask, button },
                    KeyAction::Up => Message::KeyUp { id, mask, button },
                    KeyAction::Repeat(count) => Message::KeyRepeat {
                        id,
                        mask,
                        count,
                        button,
                    },
                };
                self.send_active(msg)
            }
        }
    }

    /// Test the current virtual cursor against screen edges and, depending on
    /// whether it stayed put, returned home, or hopped to another client,
    /// produce the right actions.
    fn try_cross(&mut self) -> Vec<ServerAction> {
        let crossing = if self.switch_locked {
            None // locked to the active screen; ignore edges
        } else {
            self.layout
                .detect_crossing(&self.active, self.cursor_x, self.cursor_y)
        };
        match crossing {
            None => {
                if self.is_remote() {
                    // Clamp against a neighbour-less edge and forward the move.
                    let (cx, cy) = self.layout.clamp_to(&self.active, self.cursor_x, self.cursor_y);
                    self.cursor_x = cx;
                    self.cursor_y = cy;
                    self.send_active(Message::MouseMove {
                        x: cx as i16,
                        y: cy as i16,
                    })
                } else {
                    Vec::new() // local move, OS owns the cursor
                }
            }
            Some(crossing) => {
                let leaving = self.active.clone();
                let leaving_was_local = leaving == self.local_screen;
                let entering = crossing.to.clone();
                self.cursor_x = crossing.entry_x;
                self.cursor_y = crossing.entry_y;

                let mut actions = Vec::new();

                // Leaving a remote screen: tell that client the cursor is gone.
                if !leaving_was_local {
                    actions.push(ServerAction::Send {
                        screen: leaving,
                        msg: Message::Leave,
                    });
                }

                self.active = entering.clone();

                if entering == self.local_screen {
                    // Control returns to the primary: release the grab and put
                    // the real cursor where it logically re-enters.
                    actions.push(ServerAction::SetGrab(false));
                    actions.push(ServerAction::WarpCursor {
                        x: crossing.entry_x,
                        y: crossing.entry_y,
                    });
                } else {
                    // Entering (or hopping to) a client screen. The grab only
                    // needs turning on when we were previously local; a
                    // client→client hop already holds it.
                    self.seq = self.seq.wrapping_add(1);
                    if leaving_was_local {
                        actions.push(ServerAction::SetGrab(true));
                    }
                    // Enter carries the entry coordinates; the client warps
                    // there itself, so we deliberately do NOT also send a
                    // MouseMove (that would inject the position twice).
                    actions.push(ServerAction::Send {
                        screen: entering.clone(),
                        msg: Message::Enter {
                            x: crossing.entry_x as i16,
                            y: crossing.entry_y as i16,
                            seq: self.seq,
                            modifiers: self.modifiers,
                        },
                    });
                }

                actions.push(ServerAction::ActiveChanged { screen: entering });
                actions
            }
        }
    }

    fn send_active(&self, msg: Message) -> Vec<ServerAction> {
        vec![ServerAction::Send {
            screen: self.active.clone(),
            msg,
        }]
    }

    fn track_modifiers(&mut self, ev: &LocalEvent) {
        if let LocalEvent::Key {
            id, action, mask, ..
        } = ev
        {
            // Trust the mask the capture layer computed, but also fold in
            // explicit modifier key transitions so Enter carries fresh state.
            self.modifiers = *mask;
            let bit = modifier_bit_for(*id);
            if bit != 0 {
                match action {
                    KeyAction::Down => self.modifiers |= bit,
                    KeyAction::Up => self.modifiers &= !bit,
                    KeyAction::Repeat(_) => {}
                }
            }
        }
    }
}

fn modifier_bit_for(id: kvm_proto::keys::KeyId) -> KeyModifierMask {
    use kvm_proto::keys::key;
    match id {
        key::SHIFT_L | key::SHIFT_R => modifier::SHIFT,
        key::CONTROL_L | key::CONTROL_R => modifier::CONTROL,
        key::ALT_L => modifier::ALT,
        key::ALT_R => modifier::ALT_GR,
        key::SUPER_L | key::SUPER_R => modifier::SUPER,
        key::META_L | key::META_R => modifier::META,
        _ => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::{Edge, ScreenSize};
    use kvm_proto::keys::key;

    fn machine() -> ServerMachine {
        let mut l = ScreenLayout::new();
        l.add_screen("srv", ScreenSize::new(1920, 1080));
        l.add_screen("lap", ScreenSize::new(1280, 800));
        l.link("srv", Edge::Right, "lap");
        ServerMachine::new(l, "srv")
    }

    #[test]
    fn starts_local() {
        let m = machine();
        assert!(!m.is_remote());
        assert_eq!(m.active_screen(), "srv");
    }

    #[test]
    fn crossing_right_edge_enters_client_and_grabs() {
        let mut m = machine();
        let actions = m.handle(LocalEvent::MotionAbs { x: 1920, y: 540 });
        assert!(m.is_remote());
        assert_eq!(m.active_screen(), "lap");
        assert_eq!(
            actions,
            vec![
                ServerAction::SetGrab(true),
                ServerAction::Send {
                    screen: "lap".into(),
                    msg: Message::Enter {
                        x: 0,
                        y: 400,
                        seq: 1,
                        modifiers: 0
                    }
                },
                ServerAction::ActiveChanged {
                    screen: "lap".into()
                },
            ]
        );
    }

    #[test]
    fn motion_on_client_forwards_absolute_position() {
        let mut m = machine();
        m.handle(LocalEvent::MotionAbs { x: 1920, y: 540 }); // enter lap at (0,400)
        let actions = m.handle(LocalEvent::MotionRel { dx: 100, dy: 50 });
        assert_eq!(
            actions,
            vec![ServerAction::Send {
                screen: "lap".into(),
                msg: Message::MouseMove { x: 100, y: 450 }
            }]
        );
    }

    #[test]
    fn buttons_and_keys_forward_while_remote() {
        let mut m = machine();
        m.handle(LocalEvent::MotionAbs { x: 1920, y: 540 });

        let a = m.handle(LocalEvent::Button {
            button: 1,
            pressed: true,
        });
        assert_eq!(a[0], ServerAction::Send {
            screen: "lap".into(),
            msg: Message::MouseDown { button: 1 }
        });

        let a = m.handle(LocalEvent::Key {
            id: 0x41,
            mask: 0,
            button: 30,
            action: KeyAction::Down,
        });
        assert_eq!(a[0], ServerAction::Send {
            screen: "lap".into(),
            msg: Message::KeyDown { id: 0x41, mask: 0, button: 30 }
        });
    }

    #[test]
    fn crossing_back_releases_grab_and_warps_home() {
        let mut m = machine();
        m.handle(LocalEvent::MotionAbs { x: 1920, y: 540 }); // now on lap at (0,400)
        let actions = m.handle(LocalEvent::MotionRel { dx: -5, dy: 0 }); // off lap's left edge
        assert!(!m.is_remote());
        assert_eq!(m.active_screen(), "srv");
        assert_eq!(
            actions,
            vec![
                ServerAction::Send {
                    screen: "lap".into(),
                    msg: Message::Leave
                },
                ServerAction::SetGrab(false),
                ServerAction::WarpCursor { x: 1919, y: 540 },
                ServerAction::ActiveChanged {
                    screen: "srv".into()
                },
            ]
        );
    }

    #[test]
    fn lock_pins_cursor_to_active_screen() {
        let mut m = machine();
        m.set_lock(true);
        // Pushing off the right edge would normally cross to lap; locked, it stays.
        let actions = m.handle(LocalEvent::MotionAbs { x: 1920, y: 540 });
        assert!(!m.is_remote(), "lock should prevent crossing");
        assert_eq!(m.active_screen(), "srv");
        assert!(actions.is_empty()); // local + clamped, nothing forwarded

        // Unlock, then the same motion crosses.
        m.set_lock(false);
        m.handle(LocalEvent::MotionAbs { x: 1920, y: 540 });
        assert!(m.is_remote());
    }

    #[test]
    fn modifier_state_is_carried_into_enter() {
        let mut m = machine();
        // Hold Control on the local screen, then cross.
        m.handle(LocalEvent::Key {
            id: key::CONTROL_L,
            mask: 0,
            button: 59,
            action: KeyAction::Down,
        });
        let actions = m.handle(LocalEvent::MotionAbs { x: 1920, y: 0 });
        let enter = actions.iter().find_map(|a| match a {
            ServerAction::Send {
                msg: Message::Enter { modifiers, .. },
                ..
            } => Some(*modifiers),
            _ => None,
        });
        assert_eq!(enter, Some(modifier::CONTROL));
    }
}
