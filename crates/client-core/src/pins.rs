//! TOFU certificate pins, one per `host:port`.

use crate::settings::{ensure_parent, pins_path};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

pub struct PinStore {
    path: PathBuf,
    pins: BTreeMap<String, String>,
}

impl PinStore {
    pub fn new() -> Self {
        Self::at(pins_path())
    }

    pub fn at(path: PathBuf) -> Self {
        let pins = std::fs::read_to_string(&path)
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default();
        Self { path, pins }
    }

    pub fn get(&self, host: &str, port: u16) -> Option<&str> {
        self.pins.get(&key(host, port)).map(String::as_str)
    }

    pub fn set(&mut self, host: &str, port: u16, fingerprint: &str) -> std::io::Result<()> {
        self.pins.insert(key(host, port), fingerprint.to_owned());
        self.save()
    }

    pub fn forget(&mut self, host: &str, port: u16) -> std::io::Result<()> {
        self.pins.remove(&key(host, port));
        self.save()
    }

    fn save(&self) -> std::io::Result<()> {
        ensure_parent(&self.path)?;
        std::fs::write(&self.path, serde_json::to_string_pretty(&self.pins)?)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Default for PinStore {
    fn default() -> Self {
        Self::new()
    }
}

fn key(host: &str, port: u16) -> String {
    format!("{host}:{port}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pins_round_trip_and_forget() {
        let dir = std::env::temp_dir().join(format!("rust-chat-pins-{}", std::process::id()));
        let path = dir.join("pins.json");
        let _ = std::fs::remove_file(&path);
        let mut pins = PinStore::at(path.clone());
        assert!(pins.get("127.0.0.1", 8444).is_none());
        pins.set("127.0.0.1", 8444, "AA:BB").unwrap();
        let loaded = PinStore::at(path);
        assert_eq!(loaded.get("127.0.0.1", 8444), Some("AA:BB"));
        let mut loaded = loaded;
        loaded.forget("127.0.0.1", 8444).unwrap();
        assert!(PinStore::at(loaded.path).get("127.0.0.1", 8444).is_none());
        let _ = std::fs::remove_dir_all(dir);
    }
}
