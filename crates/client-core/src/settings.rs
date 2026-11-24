//! Client settings, the remembered connection and the OS secret store.

use directories::BaseDirs;
use keyring::Entry;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

pub const APP_DIR: &str = ".rust-chat";
pub const SERVICE: &str = "rust-chat";
pub const ACCOUNT: &str = "server-password";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ClientSettings {
    pub schema: u32,
    pub host: String,
    pub port: u16,
    pub tls: bool,
    pub name: String,
    /// Base64 of the 32-byte identity advertised in `hello`. Generated on first run.
    pub identity: String,
    pub remember_password: bool,
    pub auto_connect: bool,
    pub last_room_id: Option<String>,
    pub keep_history: bool,
    pub check_for_updates: bool,
    pub skipped_version: Option<String>,
}

impl Default for ClientSettings {
    fn default() -> Self {
        Self {
            schema: 1,
            host: "127.0.0.1".into(),
            port: 8444,
            tls: true,
            name: String::new(),
            identity: String::new(),
            remember_password: false,
            auto_connect: false,
            last_room_id: None,
            keep_history: true,
            check_for_updates: true,
            skipped_version: None,
        }
    }
}

pub fn app_dir() -> PathBuf {
    BaseDirs::new()
        .map(|dirs| dirs.home_dir().join(APP_DIR))
        .unwrap_or_else(|| PathBuf::from(APP_DIR))
}

pub struct SettingsStore {
    path: PathBuf,
}

impl SettingsStore {
    pub fn new() -> Self {
        Self::at(app_dir().join("settings.json"))
    }

    pub fn at(path: PathBuf) -> Self {
        Self { path }
    }

    pub fn load(&self) -> ClientSettings {
        let mut settings: ClientSettings = std::fs::read_to_string(&self.path)
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default();
        if settings.identity.is_empty() {
            settings.identity = base64_encode(&random_bytes(32));
            let _ = self.save(&settings);
        }
        settings
    }

    pub fn save(&self, settings: &ClientSettings) -> std::io::Result<()> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let text = serde_json::to_string_pretty(settings)?;
        std::fs::write(&self.path, text)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&self.path, std::fs::Permissions::from_mode(0o600));
        }
        Ok(())
    }
}

impl Default for SettingsStore {
    fn default() -> Self {
        Self::new()
    }
}

pub trait SecretStore: Send + Sync {
    fn available(&self) -> bool;
    fn read(&self) -> Option<String>;
    fn write(&self, secret: &str) -> Result<(), String>;
    fn clear(&self) -> Result<(), String>;
}

pub struct KeyringStore;

impl KeyringStore {
    fn entry() -> Result<Entry, String> {
        Entry::new(SERVICE, ACCOUNT).map_err(|error| error.to_string())
    }
}

impl SecretStore for KeyringStore {
    fn available(&self) -> bool {
        Self::entry().is_ok()
    }

    fn read(&self) -> Option<String> {
        Self::entry().ok()?.get_password().ok()
    }

    fn write(&self, secret: &str) -> Result<(), String> {
        Self::entry()?
            .set_password(secret)
            .map_err(|error| error.to_string())
    }

    fn clear(&self) -> Result<(), String> {
        match Self::entry()?.delete_credential() {
            Ok(()) => Ok(()),
            Err(keyring::Error::NoEntry) => Ok(()),
            Err(error) => Err(error.to_string()),
        }
    }
}

/// Settings plus the remembered password, following the same rules as the Kotlin client:
/// a password is only ever stored when the user asked for it and the OS store exists.
pub struct ConnectionMemory {
    store: SettingsStore,
    secret: Box<dyn SecretStore>,
}

impl ConnectionMemory {
    pub fn new() -> Self {
        Self {
            store: SettingsStore::new(),
            secret: Box::new(KeyringStore),
        }
    }

    pub fn can_remember_password(&self) -> bool {
        self.secret.available()
    }

    pub fn load(&self) -> (ClientSettings, Option<String>) {
        let settings = self.store.load();
        let password = if settings.remember_password {
            self.secret.read()
        } else {
            None
        };
        (settings, password)
    }

    pub fn save(&self, settings: &ClientSettings, password: Option<&str>) -> Result<(), String> {
        let remember = settings.remember_password && self.secret.available();
        let mut settings = settings.clone();
        settings.remember_password = remember;
        self.store
            .save(&settings)
            .map_err(|error| error.to_string())?;
        if remember {
            if let Some(password) = password {
                self.secret.write(password)?;
            }
        } else {
            self.secret.clear()?;
        }
        Ok(())
    }

    pub fn clear_saved_server(&self) -> Result<(), String> {
        self.store
            .save(&ClientSettings {
                identity: self.store.load().identity,
                ..ClientSettings::default()
            })
            .map_err(|error| error.to_string())?;
        self.secret.clear()
    }
}

impl Default for ConnectionMemory {
    fn default() -> Self {
        Self::new()
    }
}

pub fn sanitize(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    for ch in name.chars() {
        if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' || ch == '.' {
            out.push(ch);
        } else {
            out.push('_');
        }
    }
    if out.is_empty() {
        out.push('_');
    }
    let hash = rust_chat_crypto::to_hex(&rust_chat_crypto::sha256(name.as_bytes()));
    format!(
        "{}-{}",
        out.chars().take(40).collect::<String>(),
        &hash[..8]
    )
}

pub fn pins_path() -> PathBuf {
    app_dir().join("pins.json")
}

pub fn ensure_parent(path: &Path) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    Ok(())
}

fn base64_encode(bytes: &[u8]) -> String {
    rust_chat_crypto::base64_encode(bytes)
}

fn random_bytes(count: usize) -> Vec<u8> {
    rust_chat_crypto::random_bytes(count)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_use_the_new_port() {
        let settings = ClientSettings::default();
        assert_eq!(settings.port, 8444);
        assert!(settings.tls);
        assert!(settings.keep_history);
    }

    #[test]
    fn settings_round_trip_tolerates_missing_fields() {
        let text = r#"{"host":"example.org","port":9000}"#;
        let settings: ClientSettings = serde_json::from_str(text).unwrap();
        assert_eq!(settings.host, "example.org");
        assert_eq!(settings.port, 9000);
        assert!(settings.tls);
        assert!(settings.keep_history);
    }

    #[test]
    fn sanitize_is_stable_and_filesystem_safe() {
        let a = sanitize("127.0.0.1:8444");
        let b = sanitize("127.0.0.1:8444");
        assert_eq!(a, b);
        assert!(!a.contains(':'));
        assert_ne!(sanitize("a/b"), sanitize("a?b"));
    }

    /// Opt-in because it writes to the real OS credential store:
    /// `cargo test -p rust-chat-client-core keyring_round_trip -- --ignored`.
    #[test]
    #[ignore]
    fn keyring_round_trip() {
        let memory = ConnectionMemory::new();
        if !memory.can_remember_password() {
            return;
        }
        let settings = ClientSettings {
            remember_password: true,
            ..ClientSettings::default()
        };
        memory.save(&settings, Some("keyring-test-secret")).unwrap();
        let (loaded, password) = memory.load();
        assert!(loaded.remember_password);
        assert_eq!(password.as_deref(), Some("keyring-test-secret"));
        memory.clear_saved_server().unwrap();
        let (_, cleared) = memory.load();
        assert_eq!(cleared, None);
    }
}
