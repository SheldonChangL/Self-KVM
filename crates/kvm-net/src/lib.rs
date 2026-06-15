//! `kvm-net` — async length-framed transport for [`kvm_proto::Message`].
//!
//! Every message is sent as a 4-byte big-endian payload length followed by the
//! payload produced by [`kvm_proto::Message::encode`]. The transport is generic
//! over any `AsyncRead`/`AsyncWrite`, so the same code carries both plain TCP
//! and (later) a TLS-wrapped stream.

mod transport;

pub use transport::{FramedConn, FramedReader, FramedWriter, NetError, MAX_FRAME_LEN};

#[cfg(feature = "tls")]
pub mod tls;
