//! End-to-end client test against the real server: connect, auth, chat, history and rejection.

use rust_chat_client_core::client::{ClientHandle, ConnectRequest};
use rust_chat_client_core::history::HistoryStore;
use rust_chat_client_core::settings::ClientSettings;
use rust_chat_client_core::{ChatSnapshot, ConnectionState};
use rust_chat_crypto::PasswordVerifier;
use rust_chat_server::config::{RoomConfig, ServerConfig, TurnConfig};
use rust_chat_server::server;
use rust_chat_server::state::Shared;
use std::sync::Arc;
use std::time::Duration;
use tokio::net::TcpListener;

const PASSWORD: &str = "client integration password";

fn shared() -> Arc<Shared> {
    let mut config = ServerConfig::default();
    config.tls.enabled = false;
    config.client_rooms_file = std::env::temp_dir()
        .join(format!("rust-chat-client-rooms-{}", std::process::id()))
        .join("rooms.json")
        .to_string_lossy()
        .into_owned();
    config.rooms = vec![RoomConfig {
        id: "lobby".into(),
        name: "Lobby".into(),
        topic: String::new(),
        voice: true,
    }];
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
    let root = std::env::temp_dir().join(format!("rust-chat-client-{tag}-{}", std::process::id()));
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

#[tokio::test]
async fn connects_chats_and_writes_history() {
    let (address, server_task) = start_server().await;
    let root = root("chat");
    let settings = ClientSettings {
        host: "127.0.0.1".into(),
        port: address.port(),
        tls: false,
        name: "alice".into(),
        ..ClientSettings::default()
    };
    let client = ClientHandle::spawn_in(settings, &tokio::runtime::Handle::current(), root.clone());
    assert!(client.connect(ConnectRequest {
        host: "127.0.0.1".into(),
        port: address.port(),
        tls: false,
        name: "alice".into(),
        password: PASSWORD.into(),
        room: None,
    }));

    let snapshot = wait_for(&client, |snapshot| {
        snapshot.connection == ConnectionState::Ready && snapshot.room_id() == Some("lobby")
    })
    .await;
    assert_eq!(snapshot.server_name, "rust-chat");
    assert_eq!(snapshot.self_id.as_deref().map(str::len), Some(18));
    assert_eq!(snapshot.rooms.len(), 1);

    assert!(client.send_chat("hello over the real socket"));
    let snapshot = wait_for(&client, |snapshot| {
        snapshot
            .messages
            .iter()
            .any(|message| message.text == "hello over the real socket")
    })
    .await;
    let message = snapshot
        .messages
        .iter()
        .find(|message| message.text == "hello over the real socket")
        .unwrap();
    assert_eq!(message.name, "alice");

    let stored = HistoryStore::at(root).load(&format!("127.0.0.1:{}", address.port()), "lobby");
    assert!(
        stored
            .iter()
            .any(|message| message.text == "hello over the real socket")
    );

    assert!(client.disconnect());
    drop(client);
    server_task.abort();
}

#[tokio::test]
async fn turn_credentials_reach_the_client_snapshot() {
    let mut config = ServerConfig::default();
    config.tls.enabled = false;
    config.turn = Some(TurnConfig {
        urls: vec!["turn:127.0.0.1:3478?transport=udp".into()],
        secret: "turn-test-secret".into(),
        ttl_seconds: 600,
    });
    config.client_rooms_file = std::env::temp_dir()
        .join(format!("rust-chat-turn-rooms-{}", std::process::id()))
        .join("rooms.json")
        .to_string_lossy()
        .into_owned();
    config.rooms = vec![RoomConfig {
        id: "lobby".into(),
        name: "Lobby".into(),
        topic: String::new(),
        voice: true,
    }];
    let verifier = PasswordVerifier::create(PASSWORD, 8, 1, 1).unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server_task = tokio::spawn(async move {
        server::serve_test(Shared::new(config, verifier).unwrap(), listener)
            .await
            .unwrap();
    });
    let settings = ClientSettings {
        host: "127.0.0.1".into(),
        port: address.port(),
        tls: false,
        name: "alice".into(),
        ..ClientSettings::default()
    };
    let client = ClientHandle::spawn_in(settings, &tokio::runtime::Handle::current(), root("turn"));
    assert!(client.connect(ConnectRequest {
        host: "127.0.0.1".into(),
        port: address.port(),
        tls: false,
        name: "alice".into(),
        password: PASSWORD.into(),
        room: None,
    }));
    let snapshot = wait_for(&client, |snapshot| snapshot.turn.is_some()).await;
    let turn = snapshot.turn.unwrap();
    assert_eq!(turn.urls[0], "turn:127.0.0.1:3478?transport=udp");
    assert!(turn.username.contains(':'));
    assert!(!turn.credential.is_empty());
    assert_eq!(turn.ttl_seconds, 600);
    assert!(client.disconnect());
    drop(client);
    server_task.abort();
}

#[tokio::test]
async fn a_wrong_password_fails_without_retrying_forever() {
    let (address, server_task) = start_server().await;
    let settings = ClientSettings {
        host: "127.0.0.1".into(),
        port: address.port(),
        tls: false,
        name: "alice".into(),
        ..ClientSettings::default()
    };
    let client =
        ClientHandle::spawn_in(settings, &tokio::runtime::Handle::current(), root("wrong"));
    assert!(client.connect(ConnectRequest {
        host: "127.0.0.1".into(),
        port: address.port(),
        tls: false,
        name: "alice".into(),
        password: "not the password".into(),
        room: None,
    }));

    let snapshot = wait_for(&client, |snapshot| {
        matches!(snapshot.connection, ConnectionState::Failed(_))
    })
    .await;
    match snapshot.connection {
        ConnectionState::Failed(failure) => {
            assert!(
                failure.summary.to_ascii_lowercase().contains("password"),
                "unexpected failure: {failure:?}"
            );
        }
        other => panic!("expected a failure, got {other:?}"),
    }
    server_task.abort();
}
