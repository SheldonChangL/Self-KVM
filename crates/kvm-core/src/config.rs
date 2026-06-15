//! Persisted configuration for the daemon and GUI.

use std::io;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::layout::{ScreenLayout, ScreenSize};

fn to_io<E: std::fmt::Display>(e: E) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, e.to_string())
}

fn default_port() -> u16 {
    kvm_proto::DEFAULT_PORT
}

/// Configuration for running as a server (primary).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerConfig {
    /// Address to bind, e.g. `0.0.0.0`.
    #[serde(default = "default_bind")]
    pub bind: String,
    #[serde(default = "default_port")]
    pub port: u16,
    /// Name of this machine's own screen within `layout`.
    pub local_screen: String,
    pub layout: ScreenLayout,
    /// Require TLS with trusted-fingerprint clients.
    #[serde(default)]
    pub tls: bool,
}

fn default_bind() -> String {
    "0.0.0.0".to_string()
}

impl ServerConfig {
    /// Write this config as pretty JSON.
    pub fn save(&self, path: impl AsRef<Path>) -> io::Result<()> {
        std::fs::write(path, serde_json::to_string_pretty(self).map_err(to_io)?)
    }

    /// Read a config from a JSON file.
    pub fn load(path: impl AsRef<Path>) -> io::Result<Self> {
        serde_json::from_str(&std::fs::read_to_string(path)?).map_err(to_io)
    }
}

impl Default for ServerConfig {
    fn default() -> Self {
        let mut layout = ScreenLayout::new();
        layout.add_screen("server", ScreenSize::new(1920, 1080));
        Self {
            bind: default_bind(),
            port: default_port(),
            local_screen: "server".into(),
            layout,
            tls: false,
        }
    }
}

/// Configuration for running as a client (secondary).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClientConfig {
    /// `host:port` of the server.
    pub server_addr: String,
    /// This client's screen name — must match a screen in the server layout.
    pub name: String,
    pub screen: ScreenSize,
    #[serde(default)]
    pub tls: bool,
}

impl ClientConfig {
    pub fn save(&self, path: impl AsRef<Path>) -> io::Result<()> {
        std::fs::write(path, serde_json::to_string_pretty(self).map_err(to_io)?)
    }

    pub fn load(path: impl AsRef<Path>) -> io::Result<Self> {
        serde_json::from_str(&std::fs::read_to_string(path)?).map_err(to_io)
    }
}

impl Default for ClientConfig {
    fn default() -> Self {
        Self {
            server_addr: format!("127.0.0.1:{}", kvm_proto::DEFAULT_PORT),
            name: "client".into(),
            screen: ScreenSize::new(1280, 800),
            tls: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn server_config_roundtrips_json() {
        let cfg = ServerConfig::default();
        let json = serde_json::to_string(&cfg).unwrap();
        let back: ServerConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(cfg, back);
    }

    #[test]
    fn client_config_roundtrips_json() {
        let cfg = ClientConfig::default();
        let json = serde_json::to_string(&cfg).unwrap();
        let back: ClientConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(cfg, back);
    }

    #[test]
    fn server_config_saves_and_loads_from_disk() {
        let mut path = std::env::temp_dir();
        path.push(format!("self-kvm-test-{}.json", std::process::id()));
        let cfg = ServerConfig::default();
        cfg.save(&path).unwrap();
        let back = ServerConfig::load(&path).unwrap();
        assert_eq!(cfg, back);
        let _ = std::fs::remove_file(&path);
    }
}
