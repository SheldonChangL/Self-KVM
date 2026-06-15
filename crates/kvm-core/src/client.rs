//! The client-side state machine.
//!
//! [`ClientMachine`] consumes protocol [`Message`]s from the server and emits
//! [`ClientAction`]s: input commands for the injection backend and protocol
//! replies for the runtime to send back. Like [`crate::server::ServerMachine`]
//! it is pure and fully testable.

use kvm_proto::{Message, PROTOCOL_MAJOR, PROTOCOL_MINOR};

use crate::events::{InputCommand, KeyAction};
use crate::layout::ScreenSize;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ClientAction {
    /// Inject this command on the local machine.
    Inject(InputCommand),
    /// Send this message back to the server.
    Reply(Message),
    /// Handshake completed.
    Connected,
    /// Cursor entered this screen.
    Entered,
    /// Cursor left this screen.
    Left,
    /// Server asked to close, or sent a fatal error.
    Disconnect(DisconnectReason),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DisconnectReason {
    ServerClosed,
    IncompatibleVersion { major: i16, minor: i16 },
    NameInUse,
    UnknownToServer,
    ProtocolViolation,
}

pub struct ClientMachine {
    name: String,
    screen: ScreenSize,
    /// True once the server has acknowledged our DeviceInfo; until then we drop
    /// mouse-move messages to avoid warping to stale coordinates.
    acked: bool,
    /// True while the cursor is on this screen.
    entered: bool,
    /// Sequence number of the most recent Enter, echoed on replies that need it.
    last_seq: u32,
}

impl ClientMachine {
    pub fn new(name: impl Into<String>, screen: ScreenSize) -> Self {
        Self {
            name: name.into(),
            screen,
            acked: false,
            entered: false,
            last_seq: 0,
        }
    }

    pub fn is_entered(&self) -> bool {
        self.entered
    }

    pub fn last_seq(&self) -> u32 {
        self.last_seq
    }

    /// Drive the machine with one message received from the server.
    pub fn handle(&mut self, msg: Message) -> Vec<ClientAction> {
        match msg {
            Message::Hello { .. } => vec![ClientAction::Reply(Message::HelloBack {
                major: PROTOCOL_MAJOR,
                minor: PROTOCOL_MINOR,
                name: self.name.clone(),
            })],
            Message::QueryInfo => vec![ClientAction::Reply(Message::DeviceInfo {
                x: 0,
                y: 0,
                w: self.screen.w as i16,
                h: self.screen.h as i16,
                warp: 0,
                mx: (self.screen.w / 2) as i16,
                my: (self.screen.h / 2) as i16,
            })],
            Message::InfoAck => {
                self.acked = true;
                vec![ClientAction::Connected]
            }
            Message::ResetOptions | Message::SetOptions { .. } => Vec::new(),
            Message::KeepAlive => vec![ClientAction::Reply(Message::KeepAlive)],
            Message::NoOp => Vec::new(),

            Message::Enter {
                x, y, seq, ..
            } => {
                self.entered = true;
                self.last_seq = seq;
                vec![
                    ClientAction::Entered,
                    ClientAction::Inject(InputCommand::MouseMoveAbs {
                        x: x as i32,
                        y: y as i32,
                    }),
                ]
            }
            Message::Leave => {
                self.entered = false;
                vec![ClientAction::Left]
            }

            Message::MouseMove { x, y } => self.inject_if_active(InputCommand::MouseMoveAbs {
                x: x as i32,
                y: y as i32,
            }),
            Message::MouseRelMove { dx, dy } => {
                self.inject_if_active(InputCommand::MouseMoveRel {
                    dx: dx as i32,
                    dy: dy as i32,
                })
            }
            Message::MouseDown { button } => self.inject_if_active(InputCommand::MouseButton {
                button,
                pressed: true,
            }),
            Message::MouseUp { button } => self.inject_if_active(InputCommand::MouseButton {
                button,
                pressed: false,
            }),
            Message::MouseWheel { x, y } => {
                self.inject_if_active(InputCommand::MouseWheel { x, y })
            }
            Message::KeyDown { id, mask, button } => self.inject_if_active(InputCommand::Key {
                id,
                mask,
                button,
                action: KeyAction::Down,
            }),
            Message::KeyUp { id, mask, button } => self.inject_if_active(InputCommand::Key {
                id,
                mask,
                button,
                action: KeyAction::Up,
            }),
            Message::KeyRepeat {
                id,
                mask,
                count,
                button,
            } => self.inject_if_active(InputCommand::Key {
                id,
                mask,
                button,
                action: KeyAction::Repeat(count),
            }),

            Message::Close => vec![ClientAction::Disconnect(DisconnectReason::ServerClosed)],
            Message::ErrIncompatible { major, minor } => vec![ClientAction::Disconnect(
                DisconnectReason::IncompatibleVersion { major, minor },
            )],
            Message::ErrBusy => vec![ClientAction::Disconnect(DisconnectReason::NameInUse)],
            Message::ErrUnknown => {
                vec![ClientAction::Disconnect(DisconnectReason::UnknownToServer)]
            }
            Message::ErrBad => {
                vec![ClientAction::Disconnect(DisconnectReason::ProtocolViolation)]
            }

            // Messages the state machine does not act on are ignored here.
            // Clipboard and file-transfer payloads are deliberately handled
            // out-of-band in the daemon (intercepted before `handle`), not in
            // this pure machine — see `kvm-daemon`.
            Message::HelloBack { .. }
            | Message::DeviceInfo { .. }
            | Message::ScreenSaver { .. }
            | Message::ClipboardGrab { .. }
            | Message::ClipboardData { .. }
            | Message::FileOffer { .. }
            | Message::FileChunk { .. }
            | Message::FileEnd { .. }
            | Message::FileAccept { .. } => Vec::new(),
        }
    }

    fn inject_if_active(&self, cmd: InputCommand) -> Vec<ClientAction> {
        // Mouse moves require an ack and an active screen; key/button events are
        // always injected once entered (they may legitimately arrive in bursts).
        let gated = matches!(
            cmd,
            InputCommand::MouseMoveAbs { .. } | InputCommand::MouseMoveRel { .. }
        );
        if self.entered && (self.acked || !gated) {
            vec![ClientAction::Inject(cmd)]
        } else {
            Vec::new()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn client() -> ClientMachine {
        ClientMachine::new("laptop", ScreenSize::new(1280, 800))
    }

    #[test]
    fn handshake_sequence() {
        let mut c = client();

        let a = c.handle(Message::Hello { major: 1, minor: 8 });
        assert_eq!(
            a,
            vec![ClientAction::Reply(Message::HelloBack {
                major: PROTOCOL_MAJOR,
                minor: PROTOCOL_MINOR,
                name: "laptop".into()
            })]
        );

        let a = c.handle(Message::QueryInfo);
        assert_eq!(
            a,
            vec![ClientAction::Reply(Message::DeviceInfo {
                x: 0,
                y: 0,
                w: 1280,
                h: 800,
                warp: 0,
                mx: 640,
                my: 400
            })]
        );

        let a = c.handle(Message::InfoAck);
        assert_eq!(a, vec![ClientAction::Connected]);
    }

    #[test]
    fn moves_dropped_until_entered_and_acked() {
        let mut c = client();
        // Not entered, not acked: dropped.
        assert!(c.handle(Message::MouseMove { x: 10, y: 10 }).is_empty());

        c.handle(Message::InfoAck);
        // Acked but not entered: still dropped.
        assert!(c.handle(Message::MouseMove { x: 10, y: 10 }).is_empty());

        let a = c.handle(Message::Enter {
            x: 5,
            y: 6,
            seq: 1,
            modifiers: 0,
        });
        assert_eq!(
            a,
            vec![
                ClientAction::Entered,
                ClientAction::Inject(InputCommand::MouseMoveAbs { x: 5, y: 6 })
            ]
        );
        assert!(c.is_entered());

        // Now moves inject.
        let a = c.handle(Message::MouseMove { x: 100, y: 200 });
        assert_eq!(
            a,
            vec![ClientAction::Inject(InputCommand::MouseMoveAbs { x: 100, y: 200 })]
        );
    }

    #[test]
    fn keepalive_is_echoed() {
        let mut c = client();
        assert_eq!(
            c.handle(Message::KeepAlive),
            vec![ClientAction::Reply(Message::KeepAlive)]
        );
    }

    #[test]
    fn leave_stops_injection() {
        let mut c = client();
        c.handle(Message::InfoAck);
        c.handle(Message::Enter {
            x: 0,
            y: 0,
            seq: 1,
            modifiers: 0,
        });
        assert_eq!(c.handle(Message::Leave), vec![ClientAction::Left]);
        assert!(!c.is_entered());
        assert!(c.handle(Message::MouseMove { x: 1, y: 1 }).is_empty());
    }

    #[test]
    fn errors_map_to_disconnect() {
        let mut c = client();
        assert_eq!(
            c.handle(Message::ErrBusy),
            vec![ClientAction::Disconnect(DisconnectReason::NameInUse)]
        );
    }
}
