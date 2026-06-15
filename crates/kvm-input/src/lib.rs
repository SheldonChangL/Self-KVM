//! `kvm-input` — the boundary between the pure state machines and the OS.
//!
//! Two traits abstract every platform concern:
//!
//! * [`Injector`] applies an [`InputCommand`] on the local machine (client side);
//! * [`InputCapture`] streams [`LocalEvent`]s from the local machine and honours
//!   a [`GrabSwitch`] so the server can suppress local input while the cursor is
//!   on a remote screen.
//!
//! A [`MockInjector`] records commands for tests, and a [`NoopInjector`] discards
//! them. The real OS backends — `rdev` capture and `enigo` injection — live
//! behind the `backends` feature so the testable core never compiles them.

pub mod clipboard;
mod inject;

pub use clipboard::{Clipboard, MockClipboard};
pub use inject::{Injector, MockInjector, NoopInjector};

use kvm_core::LocalEvent;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

/// A shared, thread-safe flag the server toggles to start/stop suppressing local
/// input. The capture backend reads it every event.
#[derive(Clone, Default)]
pub struct GrabSwitch(Arc<AtomicBool>);

impl GrabSwitch {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set(&self, on: bool) {
        self.0.store(on, Ordering::SeqCst);
    }

    pub fn enabled(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }
}

/// A sink for captured events. The runtime supplies a closure that forwards into
/// its async channel; keeping it a plain closure means `kvm-input` needs no
/// async runtime dependency.
pub type EventSink = Box<dyn Fn(LocalEvent) + Send + Sync>;

#[derive(Debug, thiserror::Error)]
pub enum InputError {
    #[error("input backend unavailable: {0}")]
    Unavailable(String),
    #[error("permission denied (grant Accessibility / Input Monitoring): {0}")]
    PermissionDenied(String),
    #[error("backend error: {0}")]
    Backend(String),
}

/// A source of local input events (the server's keyboard/mouse).
pub trait InputCapture: Send {
    /// Begin capturing. Implementations typically spawn their own OS thread and
    /// push events through `sink`; while [`GrabSwitch::enabled`] they suppress
    /// the event from reaching the local system and report motion as relative.
    fn start(&self, sink: EventSink, grab: GrabSwitch) -> Result<(), InputError>;
}

#[cfg(feature = "backends")]
pub mod enigo_backend;
#[cfg(feature = "backends")]
pub mod rdev_backend;

/// Best-effort detection of the primary display's size, used to auto-populate
/// screen geometry so the operator need not hand-write resolutions. Returns
/// `None` when it can't be determined — no `backends` feature, a headless host,
/// or a session (e.g. some Wayland setups) that won't report it — in which case
/// the caller falls back to a configured/default size.
#[cfg(feature = "backends")]
pub fn primary_display_size() -> Option<(i32, i32)> {
    let displays = display_info::DisplayInfo::all().ok()?;
    let chosen = displays
        .iter()
        .find(|d| d.is_primary)
        .or_else(|| displays.first())?;
    Some((chosen.width as i32, chosen.height as i32))
}

#[cfg(not(feature = "backends"))]
pub fn primary_display_size() -> Option<(i32, i32)> {
    None
}
