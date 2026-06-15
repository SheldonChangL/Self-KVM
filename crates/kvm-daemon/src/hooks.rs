//! Server-side local I/O hooks: toggling the input grab and warping the local
//! cursor when control returns to the primary screen.

use std::sync::{Arc, Mutex};

use kvm_input::GrabSwitch;

/// The server's local-machine side effects. In production this drives the
/// capture grab and an injection backend; in tests a recording impl captures
/// the calls.
pub trait ServerHooks: Send {
    fn set_grab(&mut self, on: bool);
    fn warp_cursor(&mut self, x: i32, y: i32);
}

/// Hooks that do nothing — useful for running headless to exercise only the
/// network/forwarding path.
#[derive(Default)]
pub struct NoopHooks;

impl ServerHooks for NoopHooks {
    fn set_grab(&mut self, _on: bool) {}
    fn warp_cursor(&mut self, _x: i32, _y: i32) {}
}

/// What a [`RecordingHooks`] observed, for test assertions.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct HookLog {
    pub grabs: Vec<bool>,
    pub warps: Vec<(i32, i32)>,
}

/// Records hook calls into a shared log.
#[derive(Clone, Default)]
pub struct RecordingHooks {
    log: Arc<Mutex<HookLog>>,
}

impl RecordingHooks {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn log(&self) -> Arc<Mutex<HookLog>> {
        Arc::clone(&self.log)
    }

    pub fn snapshot(&self) -> HookLog {
        self.log.lock().unwrap().clone()
    }
}

impl ServerHooks for RecordingHooks {
    fn set_grab(&mut self, on: bool) {
        self.log.lock().unwrap().grabs.push(on);
    }
    fn warp_cursor(&mut self, x: i32, y: i32) {
        self.log.lock().unwrap().warps.push((x, y));
    }
}

/// Production hooks: flip the shared [`GrabSwitch`] the capture backend reads,
/// and (optionally) warp the cursor via an injection callback.
pub struct LiveHooks {
    grab: GrabSwitch,
    warp: Box<dyn FnMut(i32, i32) + Send>,
}

impl LiveHooks {
    pub fn new(grab: GrabSwitch, warp: Box<dyn FnMut(i32, i32) + Send>) -> Self {
        Self { grab, warp }
    }
}

impl ServerHooks for LiveHooks {
    fn set_grab(&mut self, on: bool) {
        self.grab.set(on);
    }
    fn warp_cursor(&mut self, x: i32, y: i32) {
        (self.warp)(x, y);
    }
}
