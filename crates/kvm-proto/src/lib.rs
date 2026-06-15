//! `kvm-proto` — the Self-KVM wire protocol.
//!
//! Pure, dependency-light (de)serialisation for a Synergy/Barrier-style KVM:
//! framed messages, a big-endian codec, and the key encoding tables. No I/O
//! lives here, which keeps the whole protocol surface exhaustively unit-testable.
//!
//! See [`message::Message`] for the message set and [`codec`] for the wire
//! primitives.

pub mod codec;
pub mod error;
pub mod keys;
pub mod message;

pub use error::{ProtoError, Result};
pub use message::{
    Message, DEFAULT_PORT, HELLO_MAGIC, KEEP_ALIVES_UNTIL_DEATH, KEEP_ALIVE_SECS, PROTOCOL_MAJOR,
    PROTOCOL_MINOR,
};
