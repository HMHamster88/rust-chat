//! Room definitions: the hand-edited config rooms plus client-created rooms persisted in JSON.

use rust_chat_protocol::RoomSummary;
use std::path::Path;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Room {
    pub id: String,
    pub name: String,
    pub topic: String,
    pub voice: bool,
    pub from_config: bool,
}

impl Room {
    pub fn summary(&self, member_count: usize, voice_cap: usize) -> RoomSummary {
        RoomSummary {
            id: self.id.clone(),
            name: self.name.clone(),
            topic: self.topic.clone(),
            member_count,
            cap: if self.voice { voice_cap } else { 0 },
        }
    }
}

/// The persisted shape of a client-created room.
pub fn load_client_rooms(path: &Path) -> anyhow::Result<Vec<Room>> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    let text = std::fs::read_to_string(path)?;
    if text.trim().is_empty() {
        return Ok(Vec::new());
    }
    let rooms: Vec<crate::config::RoomConfig> = serde_json::from_str(&text)?;
    Ok(rooms
        .into_iter()
        .map(|room| Room {
            id: room.id,
            name: room.name,
            topic: room.topic,
            voice: room.voice,
            from_config: false,
        })
        .collect())
}

pub fn save_client_rooms(path: &Path, rooms: &[Room]) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let configs: Vec<crate::config::RoomConfig> = rooms
        .iter()
        .filter(|room| !room.from_config)
        .map(|room| crate::config::RoomConfig {
            id: room.id.clone(),
            name: room.name.clone(),
            topic: room.topic.clone(),
            voice: room.voice,
        })
        .collect();
    let text = serde_json::to_string_pretty(&configs)?;
    std::fs::write(path, text)?;
    Ok(())
}

/// `dev room` → `dev-room-1a2b`; lowercase ASCII, dashes, a short random suffix.
pub fn slugify(name: &str) -> String {
    let mut base: String = name
        .chars()
        .flat_map(|ch| ch.to_lowercase())
        .map(|ch| if ch.is_ascii_alphanumeric() { ch } else { '-' })
        .collect();
    while base.contains("--") {
        base = base.replace("--", "-");
    }
    let base = base.trim_matches('-');
    let base = if base.is_empty() { "room" } else { base };
    let base: String = base.chars().take(40).collect();
    let suffix = format!("{:04x}", rand::random::<u16>());
    format!("{base}-{suffix}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugs_are_safe_and_unique_enough() {
        assert_eq!(slugify("Dev Room").split('-').next(), Some("dev"));
        assert!(slugify("hello/world").starts_with("hello-world-"));
        let a = slugify("room");
        let b = slugify("room");
        assert_ne!(a, b);
        assert!(a.len() <= 45);
    }

    #[test]
    fn rooms_round_trip_through_json() {
        let dir = std::env::temp_dir().join(format!(
            "rust-chat-rooms-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let path = dir.join("rooms.json");
        let rooms = vec![Room {
            id: "dev-1".into(),
            name: "Dev".into(),
            topic: "topic".into(),
            voice: true,
            from_config: false,
        }];
        save_client_rooms(&path, &rooms).unwrap();
        let loaded = load_client_rooms(&path).unwrap();
        assert_eq!(loaded, rooms);
        let _ = std::fs::remove_dir_all(dir);
    }
}
