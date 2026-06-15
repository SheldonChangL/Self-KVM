//! `kvm-core` — Self-KVM domain logic.
//!
//! Pure, I/O-free building blocks: the [`layout`] graph and edge-crossing
//! detection, the [`server`] routing brain, the [`client`] receiver, the
//! normalised input [`events`], and persisted [`config`] types. Everything here
//! is a deterministic state machine, which is what lets the protocol and
//! switching behaviour be exhaustively unit-tested without a display or input
//! permissions.

pub mod client;
pub mod config;
pub mod events;
pub mod file_transfer;
pub mod layout;
pub mod server;

pub use client::{ClientAction, ClientMachine, DisconnectReason};
pub use config::{ClientConfig, ServerConfig};
pub use events::{InputCommand, KeyAction, LocalEvent};
pub use file_transfer::{
    chunk_count, plan_messages, sanitize_filename, FileError, FileReassembler, DEFAULT_CHUNK_SIZE,
    MAX_CHUNK_SIZE,
};
pub use layout::{Crossing, Edge, Neighbors, ScreenLayout, ScreenNode, ScreenSize};
pub use server::{ServerAction, ServerMachine};
