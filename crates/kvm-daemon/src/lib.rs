//! `kvm-daemon` — the runtime that turns the pure pieces into a running KVM.
//!
//! [`server::ServerRuntime`] accepts client connections, performs the handshake,
//! and drives a [`kvm_core::ServerMachine`] from a stream of captured input
//! events, forwarding the resulting protocol messages to the right client.
//! [`client::ClientRuntime`] connects to a server and drives a
//! [`kvm_core::ClientMachine`], injecting received input through an
//! [`kvm_input::Injector`].
//!
//! Both runtimes take their I/O edges (captured events, injection, grab/warp
//! hooks) as parameters, which is what lets the end-to-end forwarding test run
//! over real localhost TCP with mock input — no display or permissions needed.

pub mod client;
#[cfg(feature = "clipboard")]
pub mod clipboard;
pub mod file_transfer;
pub mod hooks;
pub mod server;

#[cfg(test)]
mod e2e;

pub use client::{ClientRuntime, ClientStatus};
pub use file_transfer::{recv_file_to_dir, send_file, serve_recv, SendOptions, DEFAULT_FILE_PORT};
pub use hooks::{NoopHooks, RecordingHooks, ServerHooks};
pub use server::{ServerRuntime, ServerStatus};
