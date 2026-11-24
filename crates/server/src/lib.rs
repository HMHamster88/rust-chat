//! Server configuration, state and the WebSocket control plane.

pub mod auth;
pub mod config;
pub mod rooms;
pub mod server;
pub mod state;
pub mod turn;

pub use config::{RoomConfig, ServerConfig, TlsConfig, TurnConfig};
pub use state::Shared;

/// Default location of the hand-editable server configuration.
pub const DEFAULT_CONFIG_PATH: &str = "config/server.toml";
