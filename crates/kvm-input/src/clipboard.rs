//! Clipboard access for cross-machine clipboard sharing.
//!
//! The protocol carries clipboard ownership (`CCLP`) and data (`DCLP`); this
//! module is the local-machine side that actually reads and writes the system
//! clipboard. A [`MockClipboard`] backs tests; [`ArboardClipboard`] (behind the
//! `backends` feature) is the real cross-platform implementation.

use crate::InputError;

/// Read/write the local text clipboard.
pub trait Clipboard {
    fn get_text(&mut self) -> Result<String, InputError>;
    fn set_text(&mut self, text: &str) -> Result<(), InputError>;
}

/// In-memory clipboard for tests.
#[derive(Clone, Default)]
pub struct MockClipboard {
    buf: std::sync::Arc<std::sync::Mutex<String>>,
}

impl MockClipboard {
    pub fn new() -> Self {
        Self::default()
    }
}

impl Clipboard for MockClipboard {
    fn get_text(&mut self) -> Result<String, InputError> {
        Ok(self.buf.lock().unwrap().clone())
    }
    fn set_text(&mut self, text: &str) -> Result<(), InputError> {
        *self.buf.lock().unwrap() = text.to_string();
        Ok(())
    }
}

/// Real system clipboard via the `arboard` crate.
#[cfg(feature = "backends")]
pub struct ArboardClipboard {
    inner: arboard::Clipboard,
}

#[cfg(feature = "backends")]
impl ArboardClipboard {
    pub fn new() -> Result<Self, InputError> {
        arboard::Clipboard::new()
            .map(|inner| Self { inner })
            .map_err(|e| InputError::Backend(format!("clipboard init: {e}")))
    }
}

#[cfg(feature = "backends")]
impl Clipboard for ArboardClipboard {
    fn get_text(&mut self) -> Result<String, InputError> {
        match self.inner.get_text() {
            Ok(t) => Ok(t),
            // An empty/non-text clipboard is not an error for our purposes.
            Err(arboard::Error::ContentNotAvailable) => Ok(String::new()),
            Err(e) => Err(InputError::Backend(format!("clipboard read: {e}"))),
        }
    }
    fn set_text(&mut self, text: &str) -> Result<(), InputError> {
        self.inner
            .set_text(text.to_owned())
            .map_err(|e| InputError::Backend(format!("clipboard write: {e}")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mock_clipboard_roundtrip() {
        let mut c = MockClipboard::new();
        assert_eq!(c.get_text().unwrap(), "");
        c.set_text("hello clipboard").unwrap();
        assert_eq!(c.get_text().unwrap(), "hello clipboard");
    }
}
