//! Peer-to-peer file transfer framing, file naming and checksums.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::io::Read;
use std::path::{Path, PathBuf};

pub const MAX_FILE_BYTES: u64 = 8 * 1024 * 1024 * 1024;
pub const MAX_FILE_NAME_CHARS: usize = 255;
pub const CHUNK_BYTES: usize = 60 * 1024;
const CHUNK_TYPE: u8 = 1;
const MAX_CHUNK_ID: usize = 64;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum FileMessage {
    Offer {
        id: String,
        name: String,
        size: u64,
        sha256: String,
        #[serde(default)]
        caption: String,
    },
    Accept {
        id: String,
    },
    Decline {
        id: String,
        #[serde(default)]
        reason: String,
    },
    Cancel {
        id: String,
        #[serde(default)]
        reason: String,
    },
    Finish {
        id: String,
    },
    Complete {
        id: String,
    },
    Error {
        id: String,
        message: String,
    },
}

impl FileMessage {
    pub fn encode(&self) -> Vec<u8> {
        serde_json::to_vec(self).unwrap_or_default()
    }

    pub fn decode(data: &[u8]) -> Option<Self> {
        serde_json::from_slice(data).ok()
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum FileFrame {
    Message(FileMessage),
    Chunk {
        id: String,
        offset: u64,
        data: Vec<u8>,
    },
}

pub fn encode_chunk(id: &str, offset: u64, data: &[u8]) -> Vec<u8> {
    let id = id.as_bytes();
    let id = &id[..id.len().min(MAX_CHUNK_ID)];
    let mut out = Vec::with_capacity(1 + 1 + id.len() + 8 + data.len());
    out.push(CHUNK_TYPE);
    out.push(id.len() as u8);
    out.extend_from_slice(id);
    out.extend_from_slice(&offset.to_le_bytes());
    out.extend_from_slice(data);
    out
}

pub fn decode_frame(data: &[u8]) -> Option<FileFrame> {
    if data.first() == Some(&b'{') {
        return FileMessage::decode(data).map(FileFrame::Message);
    }
    if data.first() != Some(&CHUNK_TYPE) || data.len() < 2 {
        return None;
    }
    let id_len = data[1] as usize;
    if id_len == 0 || id_len > MAX_CHUNK_ID || data.len() < 2 + id_len + 8 {
        return None;
    }
    let id = String::from_utf8(data[2..2 + id_len].to_vec()).ok()?;
    let offset = u64::from_le_bytes(data[2 + id_len..2 + id_len + 8].try_into().ok()?);
    let payload = &data[2 + id_len + 8..];
    Some(FileFrame::Chunk {
        id,
        offset,
        data: payload.to_vec(),
    })
}

/// Strips directories, control characters and trailing dots/spaces; caps the length.
pub fn sanitize_file_name(name: &str) -> Option<String> {
    let base = name
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(name)
        .trim()
        .trim_end_matches(['.', ' ']);
    if base.is_empty() || base == "." || base == ".." {
        return None;
    }
    let mut out: String = base
        .chars()
        .map(|ch| {
            if ch.is_control() || matches!(ch, '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*')
            {
                '_'
            } else {
                ch
            }
        })
        .collect();
    if out.chars().count() > MAX_FILE_NAME_CHARS {
        let extension = Path::new(&out)
            .extension()
            .map(|ext| format!(".{}", ext.to_string_lossy()))
            .unwrap_or_default();
        let keep = MAX_FILE_NAME_CHARS.saturating_sub(extension.chars().count());
        let mut truncated: String = out.chars().take(keep).collect();
        truncated.push_str(&extension);
        out = truncated;
    }
    Some(out)
}

/// Adds ` (1)`, ` (2)`, … before the extension until the path is free.
pub fn unique_path(dir: &Path, name: &str) -> PathBuf {
    let candidate = dir.join(name);
    if !candidate.exists() {
        return candidate;
    }
    let path = Path::new(name);
    let stem = path
        .file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .unwrap_or_else(|| name.to_owned());
    let extension = path
        .extension()
        .map(|ext| format!(".{}", ext.to_string_lossy()))
        .unwrap_or_default();
    for n in 1..=999 {
        let candidate = dir.join(format!("{stem} ({n}){extension}"));
        if !candidate.exists() {
            return candidate;
        }
    }
    dir.join(format!("{stem} (overflow){extension}"))
}

/// The user's Downloads folder, or a temporary fallback, after a writability check.
pub fn download_dir() -> Result<PathBuf, String> {
    let dir = if let Ok(overridden) = std::env::var("RUST_CHAT_DOWNLOADS_DIR") {
        PathBuf::from(overridden)
    } else {
        directories::UserDirs::new()
            .and_then(|dirs| dirs.download_dir().map(Path::to_path_buf))
            .or_else(|| directories::UserDirs::new().map(|dirs| dirs.home_dir().join("Downloads")))
            .ok_or_else(|| "no home directory for downloads".to_owned())?
    };
    ensure_download_dir(&dir)
}

/// Creates the folder and proves it is writable, so a bad choice fails at accept time with a
/// readable message instead of halfway through a transfer.
pub fn ensure_download_dir(dir: &Path) -> Result<PathBuf, String> {
    std::fs::create_dir_all(dir)
        .map_err(|error| format!("cannot create {}: {error}", dir.display()))?;
    let probe = dir.join(format!(".rust-chat-write-{}", std::process::id()));
    std::fs::write(&probe, b"ok")
        .map_err(|error| format!("{} is not writable: {error}", dir.display()))?;
    let _ = std::fs::remove_file(&probe);
    Ok(dir.to_path_buf())
}

/// File settings shared with the voice session, which accepts transfers on another thread.
#[derive(Debug, Default)]
pub struct FilePrefs {
    downloads: std::sync::Mutex<Option<PathBuf>>,
    open_when_finished: std::sync::atomic::AtomicBool,
}

impl FilePrefs {
    pub fn new(downloads: Option<PathBuf>, open_when_finished: bool) -> std::sync::Arc<Self> {
        std::sync::Arc::new(Self {
            downloads: std::sync::Mutex::new(downloads),
            open_when_finished: std::sync::atomic::AtomicBool::new(open_when_finished),
        })
    }

    pub fn set_downloads(&self, dir: Option<PathBuf>) {
        if let Ok(mut slot) = self.downloads.lock() {
            *slot = dir;
        }
    }

    pub fn set_open_when_finished(&self, value: bool) {
        self.open_when_finished
            .store(value, std::sync::atomic::Ordering::Relaxed);
    }

    pub fn open_when_finished(&self) -> bool {
        self.open_when_finished
            .load(std::sync::atomic::Ordering::Relaxed)
    }

    /// The configured folder when it is still usable, otherwise the OS default.
    pub fn downloads_dir(&self) -> Result<PathBuf, String> {
        let configured = self.downloads.lock().ok().and_then(|dir| dir.clone());
        match configured {
            Some(dir) => ensure_download_dir(&dir).or_else(|_| download_dir()),
            None => download_dir(),
        }
    }
}

pub fn sha256_file(path: &Path) -> Result<String, String> {
    let mut file = std::fs::File::open(path).map_err(|error| error.to_string())?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; 256 * 1024];
    loop {
        let read = file.read(&mut buffer).map_err(|error| error.to_string())?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hex(&hasher.finalize()))
}

pub fn hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

pub fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chunks_round_trip_with_offsets() {
        let frame = encode_chunk("abc123", 4096, b"payload");
        assert_eq!(
            decode_frame(&frame),
            Some(FileFrame::Chunk {
                id: "abc123".into(),
                offset: 4096,
                data: b"payload".to_vec(),
            })
        );
        let offer = FileMessage::Offer {
            id: "abc".into(),
            name: "photo.png".into(),
            size: 10,
            sha256: "00".into(),
            caption: "hi".into(),
        };
        assert_eq!(
            decode_frame(&offer.encode()),
            Some(FileFrame::Message(offer))
        );
    }

    #[test]
    fn names_strip_paths_and_control_characters() {
        assert_eq!(
            sanitize_file_name("../../etc/passwd").as_deref(),
            Some("passwd")
        );
        assert_eq!(
            sanitize_file_name(r"C:\Windows\System32\evil.exe").as_deref(),
            Some("evil.exe")
        );
        assert_eq!(sanitize_file_name(".."), None);
        assert_eq!(sanitize_file_name("a:b?.txt").as_deref(), Some("a_b_.txt"));
        assert!(sanitize_file_name(&"x".repeat(400)).unwrap().len() <= MAX_FILE_NAME_CHARS);
    }

    #[test]
    fn unique_paths_get_numeric_suffixes() {
        let dir = std::env::temp_dir().join(format!("rust-chat-files-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let first = unique_path(&dir, "photo.png");
        std::fs::write(&first, b"x").unwrap();
        let second = unique_path(&dir, "photo.png");
        assert_eq!(
            second.file_name().unwrap().to_string_lossy(),
            "photo (1).png"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn sizes_are_human_readable() {
        assert_eq!(human_size(500), "500 B");
        assert_eq!(human_size(1536), "1.5 KiB");
        assert_eq!(human_size(5 * 1024 * 1024), "5.0 MiB");
    }

    #[test]
    fn hashing_matches_a_known_vector() {
        let path = std::env::temp_dir().join(format!("rust-chat-hash-{}", std::process::id()));
        std::fs::write(&path, b"abc").unwrap();
        assert_eq!(
            sha256_file(&path).unwrap(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        let _ = std::fs::remove_file(path);
    }
}
