//! `config/server.toml`. Keys are camelCase, matching the Kotlin server's YAML so operators
//! can translate a deployment by hand.

use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ServerConfig {
    pub server_name: String,
    pub motd: String,
    pub host: String,
    pub port: u16,
    pub verifier_file: String,
    pub client_rooms_file: String,
    pub max_users_per_room: usize,
    pub max_chat_bytes: usize,
    pub allow_client_rooms: bool,
    pub max_rooms: usize,
    pub tls: TlsConfig,
    pub rooms: Vec<RoomConfig>,
    pub turn: Option<TurnConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct TlsConfig {
    pub enabled: bool,
    pub cert_file: String,
    pub key_file: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RoomConfig {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub topic: String,
    #[serde(default = "default_true")]
    pub voice: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TurnConfig {
    pub urls: Vec<String>,
    pub secret: String,
    #[serde(default = "default_turn_ttl")]
    pub ttl_seconds: u64,
}

fn default_true() -> bool {
    true
}

fn default_turn_ttl() -> u64 {
    3600
}

impl Default for TlsConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            cert_file: "config/tls/cert.pem".into(),
            key_file: "config/tls/key.pem".into(),
        }
    }
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            server_name: "rust-chat".into(),
            motd: "Welcome".into(),
            host: "0.0.0.0".into(),
            port: 8444,
            verifier_file: "config/verifier".into(),
            client_rooms_file: "config/rooms.json".into(),
            max_users_per_room: rust_chat_protocol::DEFAULT_MAX_MEMBERS_PER_ROOM,
            max_chat_bytes: rust_chat_protocol::MAX_CHAT_BYTES,
            allow_client_rooms: true,
            max_rooms: rust_chat_protocol::DEFAULT_MAX_ROOMS,
            tls: TlsConfig::default(),
            rooms: vec![
                RoomConfig {
                    id: "lobby".into(),
                    name: "Lobby".into(),
                    topic: String::new(),
                    voice: true,
                },
                RoomConfig {
                    id: "dev".into(),
                    name: "Dev".into(),
                    topic: String::new(),
                    voice: true,
                },
            ],
            turn: None,
        }
    }
}

impl ServerConfig {
    pub fn load(path: &Path) -> anyhow::Result<Self> {
        let text = std::fs::read_to_string(path)
            .map_err(|error| anyhow::anyhow!("cannot read {}: {error}", path.display()))?;
        let config: Self = toml::from_str(&text)
            .map_err(|error| anyhow::anyhow!("cannot parse {}: {error}", path.display()))?;
        config.validate()?;
        Ok(config)
    }

    pub fn save(&self, path: &Path) -> anyhow::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let text = toml::to_string_pretty(self)?;
        std::fs::write(path, text)?;
        Ok(())
    }

    pub fn validate(&self) -> anyhow::Result<()> {
        anyhow::ensure!(self.port > 0, "port must not be 0");
        anyhow::ensure!(
            !self.server_name.trim().is_empty(),
            "serverName must not be empty"
        );
        anyhow::ensure!(!self.rooms.is_empty(), "at least one room is required");
        anyhow::ensure!(
            self.max_chat_bytes > 0 && self.max_chat_bytes <= rust_chat_protocol::MAX_FRAME_BYTES,
            "maxChatBytes must be within 1..={}",
            rust_chat_protocol::MAX_FRAME_BYTES
        );
        anyhow::ensure!(
            self.max_users_per_room > 0,
            "maxUsersPerRoom must be greater than 0"
        );
        if self.allow_client_rooms {
            anyhow::ensure!(self.max_rooms > 0, "maxRooms must be greater than 0");
        }
        for room in &self.rooms {
            anyhow::ensure!(
                !room.id.trim().is_empty() && !room.name.trim().is_empty(),
                "every room needs an id and a name"
            );
            anyhow::ensure!(
                room.topic.chars().count() <= rust_chat_protocol::MAX_TOPIC_CHARS,
                "room `{}` topic is too long",
                room.id
            );
        }
        Ok(())
    }

    /// Room ids are unique case-insensitively, matching the create-room rule.
    pub fn find_room(&self, id: &str) -> Option<&RoomConfig> {
        self.rooms
            .iter()
            .find(|room| room.id.eq_ignore_ascii_case(id))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_round_trips_toml() {
        let config = ServerConfig::default();
        let text = toml::to_string_pretty(&config).unwrap();
        let parsed: ServerConfig = toml::from_str(&text).unwrap();
        assert_eq!(parsed.server_name, "rust-chat");
        assert_eq!(parsed.port, 8444);
        assert_eq!(parsed.rooms.len(), 2);
        assert_eq!(parsed.rooms[0].id, "lobby");
        assert!(parsed.tls.enabled);
        assert!(parsed.turn.is_none());
    }

    #[test]
    fn camel_case_keys_are_used() {
        let text = toml::to_string_pretty(&ServerConfig::default()).unwrap();
        assert!(text.contains("serverName"));
        assert!(text.contains("verifierFile"));
        assert!(text.contains("maxUsersPerRoom"));
    }

    #[test]
    fn validation_rejects_impossible_values() {
        let config = ServerConfig {
            port: 0,
            ..ServerConfig::default()
        };
        assert!(config.validate().is_err());

        let config = ServerConfig {
            rooms: Vec::new(),
            ..ServerConfig::default()
        };
        assert!(config.validate().is_err());

        let config = ServerConfig {
            max_chat_bytes: rust_chat_protocol::MAX_FRAME_BYTES + 1,
            ..ServerConfig::default()
        };
        assert!(config.validate().is_err());
    }

    #[test]
    fn room_lookup_is_case_insensitive() {
        let config = ServerConfig::default();
        assert!(config.find_room("LOBBY").is_some());
        assert!(config.find_room("missing").is_none());
    }
}
