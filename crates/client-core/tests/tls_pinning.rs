//! TOFU pinning against a real TLS server: prompt, pin, reconnect without prompt, changed key.

use rcgen::{CertificateParams, KeyPair};
use rust_chat_client_core::client::{ClientHandle, ConnectRequest};
use rust_chat_client_core::pins::PinStore;
use rust_chat_client_core::settings::ClientSettings;
use rust_chat_client_core::{ChatSnapshot, ConnectionState};
use rust_chat_crypto::{PasswordVerifier, certificate_fingerprint};
use rust_chat_server::config::{RoomConfig, ServerConfig};
use rust_chat_server::server;
use rust_chat_server::state::Shared;
use std::time::Duration;

const PASSWORD: &str = "tls pinning password";

async fn wait_for(
    client: &ClientHandle,
    predicate: impl Fn(&ChatSnapshot) -> bool,
) -> ChatSnapshot {
    for _ in 0..400 {
        let snapshot = client.snapshot();
        if predicate(&snapshot) {
            return snapshot;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("timed out; last snapshot: {:?}", client.snapshot());
}

fn tls_material(dir: &std::path::Path) -> String {
    std::fs::create_dir_all(dir).unwrap();
    let name = vec!["localhost".to_owned(), "127.0.0.1".to_owned()];
    let params = CertificateParams::new(name).unwrap();
    let key = KeyPair::generate().unwrap();
    let certificate = params.self_signed(&key).unwrap();
    std::fs::write(dir.join("cert.pem"), certificate.pem()).unwrap();
    std::fs::write(dir.join("key.pem"), key.serialize_pem()).unwrap();
    certificate_fingerprint(certificate.der().as_ref())
}

fn free_port() -> u16 {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.local_addr().unwrap().port()
}

#[tokio::test]
async fn tofu_prompts_once_then_remembers_and_rejects_a_changed_key() {
    let root = std::env::temp_dir().join(format!("rust-chat-tls-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let fingerprint = tls_material(&root.join("tls"));
    let port = free_port();
    let config = ServerConfig {
        host: "127.0.0.1".into(),
        port,
        tls: rust_chat_server::config::TlsConfig {
            enabled: true,
            cert_file: root.join("tls/cert.pem").to_string_lossy().into_owned(),
            key_file: root.join("tls/key.pem").to_string_lossy().into_owned(),
        },
        client_rooms_file: root.join("rooms.json").to_string_lossy().into_owned(),
        rooms: vec![RoomConfig {
            id: "lobby".into(),
            name: "Lobby".into(),
            topic: String::new(),
            voice: true,
        }],
        ..ServerConfig::default()
    };
    let verifier = PasswordVerifier::create(PASSWORD, 8, 1, 1).unwrap();
    let shared = Shared::new(config, verifier).unwrap();
    let server_task = tokio::spawn(async move {
        let _ = server::run(shared).await;
    });
    tokio::time::sleep(Duration::from_millis(600)).await;

    let settings = ClientSettings {
        host: "127.0.0.1".into(),
        port,
        tls: true,
        name: "alice".into(),
        ..ClientSettings::default()
    };
    let request = ConnectRequest {
        host: "127.0.0.1".into(),
        port,
        tls: true,
        name: "alice".into(),
        password: PASSWORD.into(),
        room: None,
    };

    let client = ClientHandle::spawn_in(
        settings.clone(),
        &tokio::runtime::Handle::current(),
        root.clone(),
    );
    client.connect(request.clone());
    let snapshot = wait_for(&client, |snapshot| snapshot.trust.is_some()).await;
    let prompt = snapshot.trust.unwrap();
    assert_eq!(prompt.fingerprint, fingerprint);
    assert!(client.trust(true));
    let snapshot = wait_for(&client, |snapshot| {
        snapshot.connection == ConnectionState::Ready && snapshot.room_id() == Some("lobby")
    })
    .await;
    assert_eq!(
        snapshot.server_fingerprint.as_deref(),
        Some(fingerprint.as_str())
    );

    assert_eq!(
        PinStore::at(root.join("pins.json")).get("127.0.0.1", port),
        Some(fingerprint.as_str())
    );
    let _ = client.disconnect();
    drop(client);
    tokio::time::sleep(Duration::from_millis(150)).await;

    // A second client with the saved pin goes straight in, with no trust prompt.
    let second = ClientHandle::spawn_in(
        settings.clone(),
        &tokio::runtime::Handle::current(),
        root.clone(),
    );
    second.connect(request.clone());
    for _ in 0..400 {
        let snapshot = second.snapshot();
        assert!(
            snapshot.trust.is_none(),
            "a saved pin must not prompt again: {snapshot:?}"
        );
        if snapshot.connection == ConnectionState::Ready && snapshot.room_id() == Some("lobby") {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(second.snapshot().connection, ConnectionState::Ready);
    let _ = second.disconnect();
    drop(second);
    tokio::time::sleep(Duration::from_millis(150)).await;

    // A pin that no longer matches the certificate is a hard failure, never a prompt.
    let mut pins = PinStore::at(root.join("pins.json"));
    pins.set("127.0.0.1", port, "00:11:22:33").unwrap();
    let third = ClientHandle::spawn_in(settings, &tokio::runtime::Handle::current(), root.clone());
    third.connect(request);
    let snapshot = wait_for(&third, |snapshot| {
        matches!(snapshot.connection, ConnectionState::Failed(_))
    })
    .await;
    match snapshot.connection {
        ConnectionState::Failed(failure) => {
            assert!(
                failure.summary.contains("certificate changed"),
                "unexpected failure: {failure:?}"
            );
        }
        other => panic!("expected a failure, got {other:?}"),
    }
    server_task.abort();
    let _ = std::fs::remove_dir_all(root);
}
