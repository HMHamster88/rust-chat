//! Settings that must take effect without a reconnect: keeping history, clearing the transcript,
//! and where received files are written.

#![cfg(windows)]

use rust_chat_client_core::client::{ClientHandle, ConnectRequest};
use rust_chat_client_core::history::HistoryStore;
use rust_chat_client_core::settings::ClientSettings;
use rust_chat_client_core::{ChatSnapshot, ConnectionState};
use rust_chat_crypto::PasswordVerifier;
use rust_chat_desktop::files::FilePrefs;
use rust_chat_server::config::{RoomConfig, ServerConfig};
use rust_chat_server::server;
use rust_chat_server::state::Shared;
use std::time::Duration;
use tokio::net::TcpListener;

const PASSWORD: &str = "settings test password";

fn shared() -> std::sync::Arc<Shared> {
    let config = ServerConfig {
        rooms: vec![RoomConfig {
            id: "lobby".into(),
            name: "Lobby".into(),
            topic: String::new(),
            voice: true,
        }],
        ..ServerConfig::default()
    };
    let verifier = PasswordVerifier::create(PASSWORD, 8, 1, 1).unwrap();
    Shared::new(config, verifier).unwrap()
}

async fn start_server() -> (std::net::SocketAddr, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let task = tokio::spawn(async move {
        server::serve_test(shared(), listener).await.unwrap();
    });
    (address, task)
}

fn root(tag: &str) -> std::path::PathBuf {
    let root =
        std::env::temp_dir().join(format!("rust-chat-settings-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    root
}

async fn wait_for(
    client: &ClientHandle,
    predicate: impl Fn(&ChatSnapshot) -> bool,
) -> ChatSnapshot {
    for _ in 0..300 {
        let snapshot = client.snapshot();
        if predicate(&snapshot) {
            return snapshot;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("timed out; last snapshot: {:?}", client.snapshot());
}

async fn connect(
    address: std::net::SocketAddr,
    name: &str,
    root: &std::path::Path,
) -> ClientHandle {
    let settings = ClientSettings {
        host: "127.0.0.1".into(),
        port: address.port(),
        tls: false,
        name: name.into(),
        ..ClientSettings::default()
    };
    let client = ClientHandle::spawn_in(settings, &tokio::runtime::Handle::current(), root.into());
    assert!(client.connect(ConnectRequest {
        host: "127.0.0.1".into(),
        port: address.port(),
        tls: false,
        name: name.into(),
        password: PASSWORD.into(),
        room: None,
    }));
    wait_for(&client, |snapshot| {
        snapshot.connection == ConnectionState::Ready && snapshot.room_id() == Some("lobby")
    })
    .await;
    client
}

async fn say(client: &ClientHandle, text: &str) {
    let before = client.snapshot().messages.len();
    assert!(client.send_chat(text));
    wait_for(client, |snapshot| snapshot.messages.len() > before).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn history_and_transcript_preferences_apply_live() {
    let (address, server_task) = start_server().await;
    let dir = root("live");
    let client = connect(address, "alice", &dir).await;
    let endpoint = format!("127.0.0.1:{}", address.port());
    let store = HistoryStore::at(dir.clone());

    say(&client, "kept on disk").await;
    let saved = wait_for(&client, |snapshot| {
        store.load(&endpoint, "lobby").len() == 1 && snapshot.messages.len() == 1
    })
    .await;
    assert_eq!(saved.messages[0].text, "kept on disk");

    // Turning history off must apply to the next message, not the next session.
    client.set_keep_history(false);
    say(&client, "not on disk").await;
    let messages = client.snapshot().messages.len();
    assert_eq!(messages, 2);
    assert_eq!(store.load(&endpoint, "lobby").len(), 1);

    // The settings screen clears the file and the in-memory transcript together.
    assert!(client.clear_messages());
    wait_for(&client, |snapshot| snapshot.messages.is_empty()).await;
    store.clear(&endpoint, "lobby").unwrap();
    assert!(store.load(&endpoint, "lobby").is_empty());

    server_task.abort();
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn file_preferences_route_downloads_and_open_flag() {
    let dir = std::env::temp_dir().join(format!("rust-chat-fileprefs-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let target = dir.join("chosen");
    let prefs = FilePrefs::new(None, false);
    assert!(!prefs.open_when_finished());

    prefs.set_open_when_finished(true);
    assert!(prefs.open_when_finished());

    prefs.set_downloads(Some(target.clone()));
    let resolved = prefs.downloads_dir().unwrap();
    assert_eq!(resolved, target);
    assert!(target.is_dir(), "the folder is created on demand");

    // A folder that cannot be used falls back to the OS default instead of failing.
    prefs.set_downloads(Some(dir.join("blocked")));
    std::fs::create_dir_all(dir.join("blocked")).unwrap();
    std::fs::write(dir.join("blocked/not-a-dir"), b"x").unwrap();
    prefs.set_downloads(Some(dir.join("blocked/not-a-dir/nested")));
    assert!(prefs.downloads_dir().is_ok());

    prefs.set_downloads(None);
    assert!(prefs.downloads_dir().is_ok());
    let _ = std::fs::remove_dir_all(&dir);
}
