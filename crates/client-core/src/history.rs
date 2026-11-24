//! Local chat history: one JSON object per line, per server and room.

use crate::settings::{app_dir, sanitize};
use rust_chat_protocol::ChatMessage;
use std::collections::HashSet;
use std::io::Write;
use std::path::{Path, PathBuf};

pub const WINDOW: usize = 500;
const COMPACT_BYTES: u64 = 512 * 1024;

pub struct HistoryStore {
    root: PathBuf,
}

impl HistoryStore {
    pub fn new() -> Self {
        Self { root: app_dir() }
    }

    pub fn at(root: PathBuf) -> Self {
        Self { root }
    }

    pub fn path(&self, server: &str, room: &str) -> PathBuf {
        self.root
            .join("history")
            .join(sanitize(server))
            .join(format!("{}.jsonl", sanitize(room)))
    }

    pub fn load(&self, server: &str, room: &str) -> Vec<ChatMessage> {
        let path = self.path(server, room);
        let Ok(text) = std::fs::read_to_string(&path) else {
            return Vec::new();
        };
        let mut seen = HashSet::new();
        let mut messages: Vec<ChatMessage> = text
            .lines()
            .filter_map(|line| serde_json::from_str::<ChatMessage>(line).ok())
            .filter(|message| seen.insert(message.message_id.clone()))
            .collect();
        if messages.len() > WINDOW {
            messages.drain(..messages.len() - WINDOW);
        }
        messages
    }

    pub fn append(&self, server: &str, room: &str, message: &ChatMessage) -> std::io::Result<()> {
        let path = self.path(server, room);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)?;
        serde_json::to_writer(&mut file, message)?;
        file.write_all(b"\n")?;
        drop(file);
        if std::fs::metadata(&path).map(|meta| meta.len()).unwrap_or(0) > COMPACT_BYTES {
            let messages = self.load(server, room);
            self.compact(&path, &messages)?;
        }
        Ok(())
    }

    pub fn clear(&self, server: &str, room: &str) -> std::io::Result<()> {
        match std::fs::remove_file(self.path(server, room)) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error),
        }
    }

    pub fn clear_all(&self, server: &str) -> std::io::Result<()> {
        let dir = self.root.join("history").join(sanitize(server));
        match std::fs::remove_dir_all(dir) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error),
        }
    }

    fn compact(&self, path: &Path, messages: &[ChatMessage]) -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut text = String::new();
        for message in messages {
            text.push_str(&serde_json::to_string(message)?);
            text.push('\n');
        }
        std::fs::write(path, text)
    }
}

impl Default for HistoryStore {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn message(id: &str) -> ChatMessage {
        ChatMessage {
            message_id: id.into(),
            room_id: "lobby".into(),
            from: "s1".into(),
            name: "alice".into(),
            text: format!("message {id}"),
            ts: 1,
            client_msg_id: None,
        }
    }

    #[test]
    fn append_load_dedupe_and_clear() {
        let dir = std::env::temp_dir().join(format!("rust-chat-history-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let store = HistoryStore::at(dir.clone());
        store.append("server", "lobby", &message("m1")).unwrap();
        store.append("server", "lobby", &message("m2")).unwrap();
        store.append("server", "lobby", &message("m1")).unwrap();
        // A damaged line must not hide the rest.
        let path = store.path("server", "lobby");
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap();
        writeln!(file, "not json").unwrap();
        drop(file);

        let loaded = store.load("server", "lobby");
        assert_eq!(loaded.len(), 2);
        assert_eq!(loaded[0].message_id, "m1");
        assert_eq!(loaded[1].message_id, "m2");

        store.clear("server", "lobby").unwrap();
        assert!(store.load("server", "lobby").is_empty());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn keeps_only_the_newest_window() {
        let dir = std::env::temp_dir().join(format!("rust-chat-window-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let store = HistoryStore::at(dir.clone());
        for index in 0..(WINDOW + 25) {
            store
                .append("server", "lobby", &message(&format!("m{index}")))
                .unwrap();
        }
        let loaded = store.load("server", "lobby");
        assert_eq!(loaded.len(), WINDOW);
        assert_eq!(
            loaded.last().unwrap().message_id,
            format!("m{}", WINDOW + 24)
        );
        let _ = std::fs::remove_dir_all(dir);
    }
}
