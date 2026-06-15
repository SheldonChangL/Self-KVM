//! Clipboard bridge: owns the (non-`Send`) system clipboard on a dedicated
//! thread and exposes a tokio-friendly change stream plus a setter.
//!
//! Compiled only with the `clipboard` feature.

use kvm_input::clipboard::ArboardClipboard;
use kvm_input::Clipboard;

/// A cloneable handle that writes text to the local clipboard.
#[derive(Clone)]
pub struct ClipboardSetter {
    tx: std::sync::mpsc::Sender<String>,
}

impl ClipboardSetter {
    pub fn set(&self, text: String) {
        let _ = self.tx.send(text);
    }
}

pub struct ClipboardBridge;

impl ClipboardBridge {
    /// Spawn a thread that both polls for local clipboard changes (emitted on
    /// the returned receiver) and applies remote writes (via the setter).
    /// Writes update the poll baseline so a remote paste is not echoed back.
    pub fn spawn() -> (tokio::sync::mpsc::UnboundedReceiver<String>, ClipboardSetter) {
        let (changes_tx, changes_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
        let (set_tx, set_rx) = std::sync::mpsc::channel::<String>();
        std::thread::Builder::new()
            .name("kvm-clipboard".into())
            .spawn(move || {
                let mut cb = match ArboardClipboard::new() {
                    Ok(c) => c,
                    Err(e) => {
                        tracing::error!("clipboard unavailable: {e}");
                        return;
                    }
                };
                let mut last = cb.get_text().unwrap_or_default();
                loop {
                    while let Ok(text) = set_rx.try_recv() {
                        let _ = cb.set_text(&text);
                        last = text;
                    }
                    if let Ok(cur) = cb.get_text() {
                        if cur != last && !cur.is_empty() {
                            last = cur.clone();
                            if changes_tx.send(cur).is_err() {
                                break;
                            }
                        }
                    }
                    std::thread::sleep(std::time::Duration::from_millis(700));
                }
            })
            .ok();
        (changes_rx, ClipboardSetter { tx: set_tx })
    }

    /// Spawn a thread that only applies remote writes (no polling). Used by the
    /// client, which receives clipboard data but does not push its own here.
    pub fn spawn_sink() -> ClipboardSetter {
        let (set_tx, set_rx) = std::sync::mpsc::channel::<String>();
        std::thread::Builder::new()
            .name("kvm-clipboard-sink".into())
            .spawn(move || {
                let mut cb = match ArboardClipboard::new() {
                    Ok(c) => c,
                    Err(e) => {
                        tracing::error!("clipboard unavailable: {e}");
                        return;
                    }
                };
                while let Ok(text) = set_rx.recv() {
                    let _ = cb.set_text(&text);
                }
            })
            .ok();
        ClipboardSetter { tx: set_tx }
    }
}
