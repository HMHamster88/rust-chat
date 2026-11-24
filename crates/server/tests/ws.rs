//! Real WebSocket integration tests: SCRAM handshake, chat, signaling and room lifecycle.

use futures_util::{SinkExt, StreamExt};
use rust_chat_crypto::PasswordVerifier;
use rust_chat_crypto::scram::{auth_message, client_proof, keys, server_signature};
use rust_chat_crypto::{base64_decode, base64_encode, random_bytes};
use rust_chat_protocol::payloads::{Auth, Challenge, Hello, Welcome};
use rust_chat_protocol::{Envelope, ErrorCode, RoomState, kind};
use rust_chat_server::config::{RoomConfig, ServerConfig};
use rust_chat_server::server;
use rust_chat_server::state::Shared;
use std::sync::Arc;
use std::time::Duration;
use tokio::net::TcpListener;
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::tungstenite::Message;

const PASSWORD: &str = "correct horse battery staple";
const NAME: &str = "alice";

struct TestClient {
    socket: WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>,
    state: RoomState,
    session_id: String,
}

async fn recv_envelope(
    socket: &mut WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>,
) -> Envelope {
    loop {
        let message = tokio::time::timeout(Duration::from_secs(5), socket.next())
            .await
            .expect("a server message arrived in time")
            .expect("the socket is still open")
            .expect("the frame is valid");
        match message {
            Message::Text(text) => {
                return Envelope::decode(text.as_bytes()).expect("a valid envelope");
            }
            Message::Ping(_) | Message::Pong(_) => continue,
            other => panic!("unexpected frame: {other:?}"),
        }
    }
}

async fn recv_kind(
    socket: &mut WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>,
    expected: &str,
) -> Envelope {
    for _ in 0..20 {
        let envelope = recv_envelope(socket).await;
        if envelope.kind == expected {
            return envelope;
        }
    }
    panic!("no `{expected}` frame arrived");
}

async fn send(
    socket: &mut WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>,
    envelope: Envelope,
) {
    let text = String::from_utf8(envelope.encode().unwrap()).unwrap();
    socket
        .send(Message::Text(text.into()))
        .await
        .expect("the frame is sent");
}

async fn handshake_until_challenge(
    socket: &mut WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>,
    name: &str,
    features: &[&str],
) -> Challenge {
    send(
        socket,
        Envelope::new(
            kind::HELLO,
            serde_json::to_value(Hello {
                protocol: rust_chat_protocol::PROTOCOL_VERSION,
                name: name.to_owned(),
                pubkey: base64_encode(&[7u8; 32]),
                features: features.iter().map(|value| (*value).to_owned()).collect(),
            })
            .unwrap(),
        ),
    )
    .await;
    let challenge = recv_kind(socket, kind::CHALLENGE).await;
    serde_json::from_value(challenge.payload).unwrap()
}

async fn complete_auth(
    socket: &mut WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>,
    name: &str,
    password: &str,
    challenge: &Challenge,
) -> (Envelope, [u8; 32]) {
    let server_nonce = base64_decode(&challenge.nonce).unwrap();
    let client_nonce = random_bytes(16);
    let auth_key = rust_chat_crypto::scram::derive_auth_key(password, &challenge.kdf).unwrap();
    let keys = keys(&auth_key);
    let message = auth_message(name, &server_nonce, &client_nonce);
    let proof = client_proof(&keys, &message);
    let expected_signature = server_signature(&keys.server_key, &message);
    send(
        socket,
        Envelope::new(
            kind::AUTH,
            serde_json::to_value(Auth {
                client_nonce: base64_encode(&client_nonce),
                proof: base64_encode(&proof),
            })
            .unwrap(),
        ),
    )
    .await;
    let welcome = recv_kind(socket, kind::WELCOME).await;
    (welcome, expected_signature)
}

async fn connect(
    address: std::net::SocketAddr,
    name: &str,
    password: &str,
    features: &[&str],
) -> TestClient {
    let (mut socket, _) = tokio_tungstenite::connect_async(format!("ws://{address}/"))
        .await
        .expect("the server accepts the websocket");
    let challenge = handshake_until_challenge(&mut socket, name, features).await;
    let (welcome_envelope, expected_signature) =
        complete_auth(&mut socket, name, password, &challenge).await;
    let welcome: Welcome = serde_json::from_value(welcome_envelope.payload.clone()).unwrap();
    let received_signature = base64_decode(&welcome.server_signature).unwrap();
    assert_eq!(
        received_signature, expected_signature,
        "the server proved the shared password"
    );
    let state = recv_kind(&mut socket, kind::ROOM_STATE).await;
    let state: RoomState = serde_json::from_value(state.payload).unwrap();
    TestClient {
        session_id: welcome.session_id.clone(),
        socket,
        state,
    }
}

async fn wrong_password_is_rejected(address: std::net::SocketAddr) {
    let (mut socket, _) = tokio_tungstenite::connect_async(format!("ws://{address}/"))
        .await
        .expect("connected");
    let challenge = handshake_until_challenge(&mut socket, NAME, &[]).await;
    let server_nonce = base64_decode(&challenge.nonce).unwrap();
    let client_nonce = random_bytes(16);
    let auth_key =
        rust_chat_crypto::scram::derive_auth_key("wrong password", &challenge.kdf).unwrap();
    let keys = keys(&auth_key);
    let message = auth_message(NAME, &server_nonce, &client_nonce);
    let proof = client_proof(&keys, &message);
    send(
        &mut socket,
        Envelope::new(
            kind::AUTH,
            serde_json::to_value(Auth {
                client_nonce: base64_encode(&client_nonce),
                proof: base64_encode(&proof),
            })
            .unwrap(),
        ),
    )
    .await;
    let envelope = recv_envelope(&mut socket).await;
    assert_eq!(envelope.kind, kind::ERROR);
    let error: rust_chat_protocol::ErrorPayload = serde_json::from_value(envelope.payload).unwrap();
    assert_eq!(error.code, ErrorCode::AuthFailed);
    assert!(error.fatal);
}

async fn two_clients_chat(address: std::net::SocketAddr) {
    let mut alice = connect(address, "alice", PASSWORD, &[]).await;
    let mut bob = connect(address, "bob", PASSWORD, &[]).await;
    assert_eq!(alice.state.room.id, "lobby");
    assert_eq!(bob.state.room.id, "lobby");

    send(
        &mut alice.socket,
        Envelope::new(
            kind::CHAT_SEND,
            serde_json::json!({ "text": "hello from alice", "clientMsgId": "c-1" }),
        ),
    )
    .await;

    let alice_message = recv_kind(&mut alice.socket, kind::CHAT_MESSAGE).await;
    let bob_message = recv_kind(&mut bob.socket, kind::CHAT_MESSAGE).await;
    let alice_payload: rust_chat_protocol::ChatMessage =
        serde_json::from_value(alice_message.payload).unwrap();
    let bob_payload: rust_chat_protocol::ChatMessage =
        serde_json::from_value(bob_message.payload).unwrap();
    for payload in [&alice_payload, &bob_payload] {
        assert_eq!(payload.text, "hello from alice");
        assert_eq!(payload.from, alice.session_id);
        assert_eq!(payload.room_id, "lobby");
    }
    assert_eq!(alice_payload.client_msg_id.as_deref(), Some("c-1"));
    assert_eq!(bob_payload.client_msg_id.as_deref(), Some("c-1"));
}

async fn signal_relay(address: std::net::SocketAddr) {
    let mut alice = connect(address, "alice", PASSWORD, &[]).await;
    let mut bob = connect(address, "bob", PASSWORD, &[]).await;
    let bob_id = bob.session_id.clone();

    send(
        &mut alice.socket,
        Envelope::new(
            kind::RTC_SIGNAL,
            serde_json::json!({
                "to": bob_id,
                "signal": { "kind": "offer", "sdp": "v=0 test" }
            }),
        ),
    )
    .await;
    let forwarded = recv_kind(&mut bob.socket, kind::RTC_SIGNAL).await;
    assert_eq!(forwarded.payload["from"], alice.session_id);
    assert_eq!(forwarded.payload["signal"]["kind"], "offer");
    assert_eq!(forwarded.payload["signal"]["sdp"], "v=0 test");

    send(
        &mut alice.socket,
        Envelope::new(
            kind::RTC_SIGNAL,
            serde_json::json!({ "to": "s_missing", "signal": { "kind": "bye" } }),
        ),
    )
    .await;
    let error = recv_kind(&mut alice.socket, kind::ERROR).await;
    let payload: rust_chat_protocol::ErrorPayload = serde_json::from_value(error.payload).unwrap();
    assert_eq!(payload.code, ErrorCode::BadTarget);
}

async fn recv_room_state_where(
    socket: &mut WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>,
    predicate: impl Fn(&RoomState) -> bool,
) -> RoomState {
    for _ in 0..40 {
        let envelope = recv_envelope(socket).await;
        if envelope.kind != kind::ROOM_STATE {
            continue;
        }
        let state: RoomState = serde_json::from_value(envelope.payload).unwrap();
        if predicate(&state) {
            return state;
        }
    }
    panic!("no matching room.state arrived");
}

async fn room_lifecycle(address: std::net::SocketAddr) {
    let mut alice = connect(address, "alice", PASSWORD, &[]).await;
    let mut bob = connect(address, "bob", PASSWORD, &[]).await;

    // Consume the lobby snapshots caused by bob joining.
    let _ = recv_room_state_where(&mut alice.socket, |state| {
        state.room.id == "lobby" && state.members.len() >= 2
    })
    .await;

    send(
        &mut alice.socket,
        Envelope::new(
            kind::ROOM_CREATE,
            serde_json::json!({ "name": "Game Night", "topic": "fun", "voice": true }),
        ),
    )
    .await;
    let created =
        recv_room_state_where(&mut alice.socket, |state| state.room.name == "Game Night").await;
    let room_id = created.room.id.clone();

    send(
        &mut bob.socket,
        Envelope::new(kind::ROOM_JOIN, serde_json::json!({ "roomId": room_id })),
    )
    .await;
    let bob_state = recv_room_state_where(&mut bob.socket, |state| state.room.id == room_id).await;
    assert!(bob_state.members.len() >= 2);

    send(
        &mut alice.socket,
        Envelope::new(kind::ROOM_LEAVE, serde_json::json!({})),
    )
    .await;
    let alice_left = recv_kind(&mut alice.socket, kind::ROOM_LEFT).await;
    let rooms = alice_left.payload["rooms"].as_array().unwrap();
    assert!(rooms.iter().any(|room| room["id"] == room_id));

    send(
        &mut bob.socket,
        Envelope::new(kind::ROOM_LEAVE, serde_json::json!({})),
    )
    .await;
    let _ = recv_kind(&mut bob.socket, kind::ROOM_LEFT).await;

    send(
        &mut bob.socket,
        Envelope::new(kind::ROOM_DELETE, serde_json::json!({ "roomId": room_id })),
    )
    .await;
    let deleted = recv_kind(&mut bob.socket, kind::ROOM_DELETED).await;
    assert_eq!(deleted.payload["roomId"], room_id);
}

async fn no_auto_room_starts_outside(address: std::net::SocketAddr) {
    let (mut socket, _) = tokio_tungstenite::connect_async(format!("ws://{address}/"))
        .await
        .expect("connected");
    let challenge = handshake_until_challenge(&mut socket, "loner", &["no-auto-room"]).await;
    let (welcome, _) = complete_auth(&mut socket, "loner", PASSWORD, &challenge).await;
    let welcome: Welcome = serde_json::from_value(welcome.payload).unwrap();
    assert!(!welcome.rooms.is_empty());
    // No room.state follows; the next frame the server sends must be something else.
    send(
        &mut socket,
        Envelope::new(kind::PING, serde_json::json!({ "ts": 42 })),
    )
    .await;
    let pong = recv_kind(&mut socket, kind::PONG).await;
    assert_eq!(pong.payload["ts"], 42);
}

fn shared() -> Arc<Shared> {
    let mut config = ServerConfig::default();
    config.tls.enabled = false;
    config.rooms = vec![
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
    ];
    config.client_rooms_file = std::env::temp_dir()
        .join(format!(
            "rust-chat-ws-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
        .join("rooms.json")
        .to_string_lossy()
        .into_owned();
    let verifier = PasswordVerifier::create(PASSWORD, 8, 1, 1).unwrap();
    Shared::new(config, verifier).unwrap()
}

async fn start_server() -> (std::net::SocketAddr, tokio::task::JoinHandle<()>) {
    let shared = shared();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let task = tokio::spawn(async move {
        server::serve_test(shared, listener).await.unwrap();
    });
    (address, task)
}

#[tokio::test]
async fn rejects_a_wrong_password() {
    let (address, task) = start_server().await;
    wrong_password_is_rejected(address).await;
    task.abort();
}

#[tokio::test]
async fn two_clients_exchange_chat() {
    let (address, task) = start_server().await;
    two_clients_chat(address).await;
    task.abort();
}

#[tokio::test]
async fn signals_are_relayed_only_inside_the_room() {
    let (address, task) = start_server().await;
    signal_relay(address).await;
    task.abort();
}

#[tokio::test]
async fn rooms_can_be_created_joined_left_and_deleted() {
    let (address, task) = start_server().await;
    room_lifecycle(address).await;
    task.abort();
}

#[tokio::test]
async fn no_auto_room_skips_the_initial_snapshot() {
    let (address, task) = start_server().await;
    no_auto_room_starts_outside(address).await;
    task.abort();
}
