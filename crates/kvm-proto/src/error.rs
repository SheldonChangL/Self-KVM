use thiserror::Error;

/// Errors raised while encoding or decoding protocol messages.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum ProtoError {
    #[error("unexpected end of buffer: needed {needed} more byte(s), had {had}")]
    UnexpectedEof { needed: usize, had: usize },

    #[error("unknown message code {0:?}")]
    UnknownCode([u8; 4]),

    #[error("malformed message {code:?}: {reason}")]
    Malformed { code: &'static str, reason: &'static str },

    #[error("declared length {declared} exceeds maximum {max}")]
    TooLong { declared: usize, max: usize },

    #[error("invalid utf-8 in string field")]
    InvalidUtf8,
}

pub type Result<T> = std::result::Result<T, ProtoError>;
