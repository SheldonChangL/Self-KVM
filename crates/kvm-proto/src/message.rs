//! Protocol messages and their wire (de)serialisation.
//!
//! A message on the wire is `[u32 big-endian payload length][payload]`. The
//! length framing itself is handled by the transport layer (see `kvm-net`);
//! this module deals only with the *payload*, which is a 4-byte ASCII opcode
//! followed by the message's fields — except the hello greeting, whose payload
//! begins with a 7-byte magic string instead of an opcode.

use crate::codec::{Reader, Writer};
use crate::error::{ProtoError, Result};
use crate::keys::{KeyButton, KeyId, KeyModifierMask};

/// Greeting magic. Synergy uses `"Synergy"`, Barrier uses `"Barrier"`; the two
/// are otherwise wire-identical. Ours is `"SelfKVM"` — exactly 7 bytes so it
/// occupies the same `%7s` slot, and is configurable for interop experiments.
pub const HELLO_MAGIC: &[u8; 7] = b"SelfKVM";

pub const DEFAULT_PORT: u16 = 24800;
pub const PROTOCOL_MAJOR: i16 = 1;
// Minor 9 adds the file-transfer messages (FOFR/FDAT/FEND/FACK); backward
// compatible — the handshake only rejects on a major mismatch.
pub const PROTOCOL_MINOR: i16 = 9;

/// Keep-alive cadence and liveness budget (seconds), protocol-level rather than
/// relying on TCP keepalive.
pub const KEEP_ALIVE_SECS: u64 = 3;
pub const KEEP_ALIVES_UNTIL_DEATH: u32 = 3;

// 4-byte opcodes.
mod code {
    pub const QUERY_INFO: &[u8; 4] = b"QINF";
    pub const DEVICE_INFO: &[u8; 4] = b"DINF";
    pub const INFO_ACK: &[u8; 4] = b"CIAK";
    pub const RESET_OPTIONS: &[u8; 4] = b"CROP";
    pub const SET_OPTIONS: &[u8; 4] = b"DSOP";
    pub const KEEP_ALIVE: &[u8; 4] = b"CALV";
    pub const NO_OP: &[u8; 4] = b"CNOP";
    pub const CLOSE: &[u8; 4] = b"CBYE";
    pub const SCREEN_SAVER: &[u8; 4] = b"CSEC";
    pub const ENTER: &[u8; 4] = b"CINN";
    pub const LEAVE: &[u8; 4] = b"COUT";
    pub const KEY_DOWN: &[u8; 4] = b"DKDN";
    pub const KEY_UP: &[u8; 4] = b"DKUP";
    pub const KEY_REPEAT: &[u8; 4] = b"DKRP";
    pub const MOUSE_DOWN: &[u8; 4] = b"DMDN";
    pub const MOUSE_UP: &[u8; 4] = b"DMUP";
    pub const MOUSE_MOVE: &[u8; 4] = b"DMMV";
    pub const MOUSE_REL_MOVE: &[u8; 4] = b"DMRM";
    pub const MOUSE_WHEEL: &[u8; 4] = b"DMWM";
    pub const CLIPBOARD_GRAB: &[u8; 4] = b"CCLP";
    pub const CLIPBOARD_DATA: &[u8; 4] = b"DCLP";
    // File transfer (bulk side-channel). Unlike clipboard, these actually use
    // the chunk fields: a file is FOFR, then N×FDAT, then FEND.
    pub const FILE_OFFER: &[u8; 4] = b"FOFR";
    pub const FILE_CHUNK: &[u8; 4] = b"FDAT";
    pub const FILE_END: &[u8; 4] = b"FEND";
    pub const FILE_ACCEPT: &[u8; 4] = b"FACK";
    pub const ERR_INCOMPATIBLE: &[u8; 4] = b"EICV";
    pub const ERR_BUSY: &[u8; 4] = b"EBSY";
    pub const ERR_UNKNOWN: &[u8; 4] = b"EUNK";
    pub const ERR_BAD: &[u8; 4] = b"EBAD";
}

/// A single protocol message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Message {
    /// Server greeting: `magic, major, minor`.
    Hello { major: i16, minor: i16 },
    /// Client reply: `magic, major, minor, client_name`.
    HelloBack {
        major: i16,
        minor: i16,
        name: String,
    },

    /// Server asks the client to report its screen geometry.
    QueryInfo,
    /// Client screen geometry: origin, size, warp zone, current cursor.
    DeviceInfo {
        x: i16,
        y: i16,
        w: i16,
        h: i16,
        warp: i16,
        mx: i16,
        my: i16,
    },
    /// Server acknowledges `DeviceInfo`. Until received, the client ignores
    /// mouse-move messages (avoids warping to stale coordinates).
    InfoAck,
    /// Reset all options to defaults.
    ResetOptions,
    /// Set options as `(id, value)` pairs flattened into one vector.
    SetOptions { opts: Vec<i32> },

    KeepAlive,
    NoOp,
    Close,
    /// Screen saver state changed (`on`).
    ScreenSaver { on: bool },

    /// Cursor entered this client's screen at `(x, y)`; `seq` is the enter
    /// sequence number used to discard stale replies; `modifiers` carries the
    /// currently-held modifier state so it can be re-synchronised.
    Enter {
        x: i16,
        y: i16,
        seq: u32,
        modifiers: KeyModifierMask,
    },
    /// Cursor left this client's screen.
    Leave,

    KeyDown {
        id: KeyId,
        mask: KeyModifierMask,
        button: KeyButton,
    },
    KeyUp {
        id: KeyId,
        mask: KeyModifierMask,
        button: KeyButton,
    },
    KeyRepeat {
        id: KeyId,
        mask: KeyModifierMask,
        count: u16,
        button: KeyButton,
    },

    /// Mouse button pressed (1=left, 2=middle, 3=right, ...).
    MouseDown { button: i8 },
    MouseUp { button: i8 },
    /// Absolute move in the client's screen coordinate space.
    MouseMove { x: i16, y: i16 },
    /// Relative move (used for captured/relative pointer modes).
    MouseRelMove { dx: i16, dy: i16 },
    /// Wheel deltas (x horizontal, y vertical).
    MouseWheel { x: i16, y: i16 },

    /// Declare ownership of a clipboard (`id`: 0=clipboard, 1=selection).
    ClipboardGrab { id: i8, seq: u32 },
    /// Clipboard payload; `mark` chunks large transfers (0=whole/start, ...).
    ClipboardData {
        id: i8,
        seq: u32,
        mark: i8,
        data: Vec<u8>,
    },

    /// Announce an incoming file. `id` correlates the transfer; `name` is the
    /// sender's base filename (UNTRUSTED — the receiver must sanitise it before
    /// touching disk); `size` is the total byte length.
    FileOffer { id: u32, name: String, size: u64 },
    /// One ordered chunk of file `id`. `seq` is the 0-based chunk index; `data`
    /// holds at most one chunk's bytes (kept well under the 4 MiB frame cap).
    FileChunk { id: u32, seq: u32, data: Vec<u8> },
    /// File `id` is complete; `count` is the number of chunks sent so the
    /// receiver can detect a lost chunk.
    FileEnd { id: u32, count: u32 },
    /// Receiver's verdict on a `FileOffer` (`accept=false` declines, e.g. the
    /// file exceeds the size limit or the user rejected it).
    FileAccept { id: u32, accept: bool },

    /// Server runs an incompatible protocol version.
    ErrIncompatible { major: i16, minor: i16 },
    /// Client name already in use.
    ErrBusy,
    /// Server does not recognise the client name.
    ErrUnknown,
    /// Protocol violation; the sender closes the connection afterwards.
    ErrBad,
}

impl Message {
    /// Serialise this message to a payload buffer (without the outer length
    /// frame, which the transport adds).
    pub fn encode(&self) -> Vec<u8> {
        match self {
            Message::Hello { major, minor } => {
                let mut w = Writer::new();
                w.raw(HELLO_MAGIC).i16(*major).i16(*minor);
                w.into_vec()
            }
            Message::HelloBack { major, minor, name } => {
                let mut w = Writer::new();
                w.raw(HELLO_MAGIC).i16(*major).i16(*minor).string(name);
                w.into_vec()
            }
            Message::QueryInfo => Writer::with_code(code::QUERY_INFO).into_vec(),
            Message::DeviceInfo {
                x,
                y,
                w,
                h,
                warp,
                mx,
                my,
            } => {
                let mut wr = Writer::with_code(code::DEVICE_INFO);
                wr.i16(*x)
                    .i16(*y)
                    .i16(*w)
                    .i16(*h)
                    .i16(*warp)
                    .i16(*mx)
                    .i16(*my);
                wr.into_vec()
            }
            Message::InfoAck => Writer::with_code(code::INFO_ACK).into_vec(),
            Message::ResetOptions => Writer::with_code(code::RESET_OPTIONS).into_vec(),
            Message::SetOptions { opts } => {
                let mut w = Writer::with_code(code::SET_OPTIONS);
                w.i32_vec(opts);
                w.into_vec()
            }
            Message::KeepAlive => Writer::with_code(code::KEEP_ALIVE).into_vec(),
            Message::NoOp => Writer::with_code(code::NO_OP).into_vec(),
            Message::Close => Writer::with_code(code::CLOSE).into_vec(),
            Message::ScreenSaver { on } => {
                let mut w = Writer::with_code(code::SCREEN_SAVER);
                w.u8(if *on { 1 } else { 0 });
                w.into_vec()
            }
            Message::Enter {
                x,
                y,
                seq,
                modifiers,
            } => {
                let mut w = Writer::with_code(code::ENTER);
                w.i16(*x).i16(*y).u32(*seq).u16(*modifiers);
                w.into_vec()
            }
            Message::Leave => Writer::with_code(code::LEAVE).into_vec(),
            Message::KeyDown { id, mask, button } => key_msg(code::KEY_DOWN, *id, *mask, *button),
            Message::KeyUp { id, mask, button } => key_msg(code::KEY_UP, *id, *mask, *button),
            Message::KeyRepeat {
                id,
                mask,
                count,
                button,
            } => {
                let mut w = Writer::with_code(code::KEY_REPEAT);
                w.u16(*id).u16(*mask).u16(*count).u16(*button);
                w.into_vec()
            }
            Message::MouseDown { button } => {
                let mut w = Writer::with_code(code::MOUSE_DOWN);
                w.i8(*button);
                w.into_vec()
            }
            Message::MouseUp { button } => {
                let mut w = Writer::with_code(code::MOUSE_UP);
                w.i8(*button);
                w.into_vec()
            }
            Message::MouseMove { x, y } => xy_msg(code::MOUSE_MOVE, *x, *y),
            Message::MouseRelMove { dx, dy } => xy_msg(code::MOUSE_REL_MOVE, *dx, *dy),
            Message::MouseWheel { x, y } => xy_msg(code::MOUSE_WHEEL, *x, *y),
            Message::ClipboardGrab { id, seq } => {
                let mut w = Writer::with_code(code::CLIPBOARD_GRAB);
                w.i8(*id).u32(*seq);
                w.into_vec()
            }
            Message::ClipboardData {
                id,
                seq,
                mark,
                data,
            } => {
                let mut w = Writer::with_code(code::CLIPBOARD_DATA);
                w.i8(*id).u32(*seq).i8(*mark).bytes(data);
                w.into_vec()
            }
            Message::FileOffer { id, name, size } => {
                let mut w = Writer::with_code(code::FILE_OFFER);
                w.u32(*id).string(name).u64(*size);
                w.into_vec()
            }
            Message::FileChunk { id, seq, data } => {
                let mut w = Writer::with_code(code::FILE_CHUNK);
                w.u32(*id).u32(*seq).bytes(data);
                w.into_vec()
            }
            Message::FileEnd { id, count } => {
                let mut w = Writer::with_code(code::FILE_END);
                w.u32(*id).u32(*count);
                w.into_vec()
            }
            Message::FileAccept { id, accept } => {
                let mut w = Writer::with_code(code::FILE_ACCEPT);
                w.u32(*id).u8(if *accept { 1 } else { 0 });
                w.into_vec()
            }
            Message::ErrIncompatible { major, minor } => {
                let mut w = Writer::with_code(code::ERR_INCOMPATIBLE);
                w.i16(*major).i16(*minor);
                w.into_vec()
            }
            Message::ErrBusy => Writer::with_code(code::ERR_BUSY).into_vec(),
            Message::ErrUnknown => Writer::with_code(code::ERR_UNKNOWN).into_vec(),
            Message::ErrBad => Writer::with_code(code::ERR_BAD).into_vec(),
        }
    }

    /// Parse a message from a payload buffer (the bytes inside one length
    /// frame).
    pub fn decode(payload: &[u8]) -> Result<Message> {
        // Hello/HelloBack begin with the 7-byte magic instead of an opcode.
        if payload.len() >= 7 && &payload[0..7] == HELLO_MAGIC {
            let mut r = Reader::new(payload);
            r.fixed(7)?;
            let major = r.i16()?;
            let minor = r.i16()?;
            return if r.is_empty() {
                Ok(Message::Hello { major, minor })
            } else {
                Ok(Message::HelloBack {
                    major,
                    minor,
                    name: r.string()?,
                })
            };
        }

        if payload.len() < 4 {
            return Err(ProtoError::UnexpectedEof {
                needed: 4,
                had: payload.len(),
            });
        }
        let mut codebuf = [0u8; 4];
        codebuf.copy_from_slice(&payload[0..4]);
        let mut r = Reader::new(&payload[4..]);

        let msg = match &codebuf {
            c if c == code::QUERY_INFO => Message::QueryInfo,
            c if c == code::DEVICE_INFO => Message::DeviceInfo {
                x: r.i16()?,
                y: r.i16()?,
                w: r.i16()?,
                h: r.i16()?,
                warp: r.i16()?,
                mx: r.i16()?,
                my: r.i16()?,
            },
            c if c == code::INFO_ACK => Message::InfoAck,
            c if c == code::RESET_OPTIONS => Message::ResetOptions,
            c if c == code::SET_OPTIONS => Message::SetOptions {
                opts: r.i32_vec()?,
            },
            c if c == code::KEEP_ALIVE => Message::KeepAlive,
            c if c == code::NO_OP => Message::NoOp,
            c if c == code::CLOSE => Message::Close,
            c if c == code::SCREEN_SAVER => Message::ScreenSaver {
                on: r.u8()? != 0,
            },
            c if c == code::ENTER => Message::Enter {
                x: r.i16()?,
                y: r.i16()?,
                seq: r.u32()?,
                modifiers: r.u16()?,
            },
            c if c == code::LEAVE => Message::Leave,
            c if c == code::KEY_DOWN => Message::KeyDown {
                id: r.u16()?,
                mask: r.u16()?,
                button: r.u16()?,
            },
            c if c == code::KEY_UP => Message::KeyUp {
                id: r.u16()?,
                mask: r.u16()?,
                button: r.u16()?,
            },
            c if c == code::KEY_REPEAT => Message::KeyRepeat {
                id: r.u16()?,
                mask: r.u16()?,
                count: r.u16()?,
                button: r.u16()?,
            },
            c if c == code::MOUSE_DOWN => Message::MouseDown { button: r.i8()? },
            c if c == code::MOUSE_UP => Message::MouseUp { button: r.i8()? },
            c if c == code::MOUSE_MOVE => Message::MouseMove {
                x: r.i16()?,
                y: r.i16()?,
            },
            c if c == code::MOUSE_REL_MOVE => Message::MouseRelMove {
                dx: r.i16()?,
                dy: r.i16()?,
            },
            c if c == code::MOUSE_WHEEL => Message::MouseWheel {
                x: r.i16()?,
                y: r.i16()?,
            },
            c if c == code::CLIPBOARD_GRAB => Message::ClipboardGrab {
                id: r.i8()?,
                seq: r.u32()?,
            },
            c if c == code::CLIPBOARD_DATA => Message::ClipboardData {
                id: r.i8()?,
                seq: r.u32()?,
                mark: r.i8()?,
                data: r.bytes()?,
            },
            c if c == code::FILE_OFFER => Message::FileOffer {
                id: r.u32()?,
                name: r.string()?,
                size: r.u64()?,
            },
            c if c == code::FILE_CHUNK => Message::FileChunk {
                id: r.u32()?,
                seq: r.u32()?,
                data: r.bytes()?,
            },
            c if c == code::FILE_END => Message::FileEnd {
                id: r.u32()?,
                count: r.u32()?,
            },
            c if c == code::FILE_ACCEPT => Message::FileAccept {
                id: r.u32()?,
                accept: r.u8()? != 0,
            },
            c if c == code::ERR_INCOMPATIBLE => Message::ErrIncompatible {
                major: r.i16()?,
                minor: r.i16()?,
            },
            c if c == code::ERR_BUSY => Message::ErrBusy,
            c if c == code::ERR_UNKNOWN => Message::ErrUnknown,
            c if c == code::ERR_BAD => Message::ErrBad,
            other => return Err(ProtoError::UnknownCode(*other)),
        };
        Ok(msg)
    }

    /// True for input-forwarding messages that only flow while the cursor is on
    /// a remote screen.
    pub fn is_input(&self) -> bool {
        matches!(
            self,
            Message::KeyDown { .. }
                | Message::KeyUp { .. }
                | Message::KeyRepeat { .. }
                | Message::MouseDown { .. }
                | Message::MouseUp { .. }
                | Message::MouseMove { .. }
                | Message::MouseRelMove { .. }
                | Message::MouseWheel { .. }
        )
    }
}

fn key_msg(code: &[u8; 4], id: KeyId, mask: KeyModifierMask, button: KeyButton) -> Vec<u8> {
    let mut w = Writer::with_code(code);
    w.u16(id).u16(mask).u16(button);
    w.into_vec()
}

fn xy_msg(code: &[u8; 4], x: i16, y: i16) -> Vec<u8> {
    let mut w = Writer::with_code(code);
    w.i16(x).i16(y);
    w.into_vec()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roundtrip(m: Message) {
        let bytes = m.encode();
        let back = Message::decode(&bytes).expect("decode");
        assert_eq!(m, back, "roundtrip mismatch for {m:?}");
    }

    #[test]
    fn roundtrip_all_variants() {
        roundtrip(Message::Hello {
            major: 1,
            minor: 8,
        });
        roundtrip(Message::HelloBack {
            major: 1,
            minor: 8,
            name: "laptop".into(),
        });
        roundtrip(Message::QueryInfo);
        roundtrip(Message::DeviceInfo {
            x: 0,
            y: 0,
            w: 1920,
            h: 1080,
            warp: 0,
            mx: 100,
            my: 200,
        });
        roundtrip(Message::InfoAck);
        roundtrip(Message::ResetOptions);
        roundtrip(Message::SetOptions {
            opts: vec![1, 2, 3, 4],
        });
        roundtrip(Message::KeepAlive);
        roundtrip(Message::NoOp);
        roundtrip(Message::Close);
        roundtrip(Message::ScreenSaver { on: true });
        roundtrip(Message::Enter {
            x: 10,
            y: 20,
            seq: 42,
            modifiers: 0x0003,
        });
        roundtrip(Message::Leave);
        roundtrip(Message::KeyDown {
            id: 0x41,
            mask: 0x0001,
            button: 30,
        });
        roundtrip(Message::KeyUp {
            id: 0x41,
            mask: 0,
            button: 30,
        });
        roundtrip(Message::KeyRepeat {
            id: 0x61,
            mask: 0,
            count: 3,
            button: 30,
        });
        roundtrip(Message::MouseDown { button: 1 });
        roundtrip(Message::MouseUp { button: 3 });
        roundtrip(Message::MouseMove { x: -5, y: 600 });
        roundtrip(Message::MouseRelMove { dx: -2, dy: 4 });
        roundtrip(Message::MouseWheel { x: 0, y: 120 });
        roundtrip(Message::ClipboardGrab { id: 0, seq: 7 });
        roundtrip(Message::ClipboardData {
            id: 1,
            seq: 7,
            mark: 0,
            data: b"copied text".to_vec(),
        });
        roundtrip(Message::FileOffer {
            id: 42,
            name: "report.pdf".into(),
            size: 5_000_000_000, // > 4 GiB exercises the u64 field
        });
        roundtrip(Message::FileChunk {
            id: 42,
            seq: 3,
            data: vec![0xABu8; 1024],
        });
        roundtrip(Message::FileEnd { id: 42, count: 17 });
        roundtrip(Message::FileAccept {
            id: 42,
            accept: true,
        });
        roundtrip(Message::ErrIncompatible {
            major: 1,
            minor: 6,
        });
        roundtrip(Message::ErrBusy);
        roundtrip(Message::ErrUnknown);
        roundtrip(Message::ErrBad);
    }

    #[test]
    fn hello_and_helloback_are_distinguished() {
        let hello = Message::Hello { major: 1, minor: 8 }.encode();
        assert!(matches!(
            Message::decode(&hello),
            Ok(Message::Hello { .. })
        ));
        // Hello payload is exactly magic(7) + 2*i16 = 11 bytes.
        assert_eq!(hello.len(), 11);
    }

    #[test]
    fn unknown_code_is_rejected() {
        let bogus = b"ZZZZ".to_vec();
        assert!(matches!(
            Message::decode(&bogus),
            Err(ProtoError::UnknownCode(_))
        ));
    }

    #[test]
    fn enter_layout_is_exact() {
        // CINN + i16 x + i16 y + u32 seq + u16 modifiers = 4 + 2 + 2 + 4 + 2 = 14
        let bytes = Message::Enter {
            x: 1,
            y: 2,
            seq: 3,
            modifiers: 4,
        }
        .encode();
        assert_eq!(&bytes[0..4], b"CINN");
        assert_eq!(bytes.len(), 14);
    }
}
