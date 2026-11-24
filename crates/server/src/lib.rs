//! Server configuration and operations that exist in M0 (init, credentials, fingerprint).
//! The WebSocket runtime lands in M1.

pub mod config;

pub use config::{RoomConfig, ServerConfig, TlsConfig, TurnConfig};

/// Default location of the hand-editable server configuration.
pub const DEFAULT_CONFIG_PATH: &str = "config/server.toml";
