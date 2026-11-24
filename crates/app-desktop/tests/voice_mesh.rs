//! End-to-end voice: two clients authenticate over WebSocket, exchange offers through the
//! server relay, and carry Opus both ways. Audio devices are replaced with channels.

#![cfg(windows)]

use rust_chat_client_core::client::{ClientHandle, ConnectRequest};
use rust_chat_client_core::settings::ClientSettings;
use rust_chat_client_core::{ChatSnapshot, ConnectionState};
use rust_chat_crypto::PasswordVerifier;
use rust_chat_desktop::voice::{self, VoiceCommand, VoiceEvent};
use rust_chat_media::audio::{FRAME_SAMPLES, sine};
use rust_chat_media_desktop::audio::{AudioControl, AudioEvent, AudioHandle, Playout};
use rust_chat_server::config::{RoomConfig, ServerConfig};
use rust_chat_server::server;
use rust_chat_server::state::Shared;
use std::sync::Arc;
use std::time::Duration;
use tokio::net::TcpListener;
use tokio::sync::mpsc;

const PASSWORD: &str = "voice mesh password";

struct FakeAudio {
    handle: AudioHandle,
    frames: mpsc::UnboundedSender<Vec<i16>>,
    _events: mpsc::UnboundedSender<AudioEvent>,
    playout: Playout,
}

fn fake_audio() -> FakeAudio {
    let (frame_tx, frames) = mpsc::unbounded_channel();
    let (event_tx, events) = mpsc::unbounded_channel();
    let playout = Playout::default();
    FakeAudio {
        handle: AudioHandle {
            frames,
            events,
            playout: playout.clone(),
            control: Arc::new(AudioControl::default()),
        },
        frames: frame_tx,
        _events: event_tx,
        playout,
    }
}

fn shared() -> Arc<Shared> {
    let mut config = ServerConfig::default();
    config.tls.enabled = false;
    config.client_rooms_file = std::env::temp_dir()
        .join(format!("rust-chat-voice-rooms-{}", std::process::id()))
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
    let root = std::env::temp_dir().join(format!("rust-chat-voice-{tag}-{}", std::process::id()));
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

async fn connect(address: std::net::SocketAddr, name: &str, tag: &str) -> ClientHandle {
    let settings = ClientSettings {
        host: "127.0.0.1".into(),
        port: address.port(),
        tls: false,
        name: name.into(),
        ..ClientSettings::default()
    };
    let client = ClientHandle::spawn_in(settings, &tokio::runtime::Handle::current(), root(tag));
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn two_clients_exchange_opus_through_the_server() {
    // Bind WebRTC to loopback so host candidates stay inside the test machine.
    // SAFETY: the variable is read by peer setup and no other test creates peers.
    unsafe {
        std::env::set_var("RUST_CHAT_MEDIA_UDP", "127.0.0.1:0");
    }
    let (address, server_task) = start_server().await;
    let alice = connect(address, "alice", "alice").await;
    let bob = connect(address, "bob", "bob").await;

    let alice_id = alice.snapshot().self_id.clone().unwrap();
    let bob_id = bob.snapshot().self_id.clone().unwrap();
    assert_ne!(alice_id, bob_id);

    let alice_audio = fake_audio();
    let bob_audio = fake_audio();
    let mut alice_voice = voice::start(
        alice.clone(),
        alice_id.clone(),
        alice_audio.handle,
        &tokio::runtime::Handle::current(),
    );
    let mut bob_voice = voice::start(
        bob.clone(),
        bob_id.clone(),
        bob_audio.handle,
        &tokio::runtime::Handle::current(),
    );

    // Announce voice and wait until both authoritative member lists see it.
    let _ = alice.set_voice(true);
    let _ = bob.set_voice(true);
    let wait = |client: &ClientHandle, other: &str| {
        let other = other.to_owned();
        let client = client.clone();
        async move {
            wait_for(&client, move |snapshot| {
                snapshot.room.as_ref().is_some_and(|room| {
                    room.members.iter().filter(|member| member.voice).count() >= 2
                        && room
                            .members
                            .iter()
                            .any(|member| member.session_id == other && member.voice)
                })
            })
            .await;
        }
    };
    tokio::join!(wait(&alice, &bob_id), wait(&bob, &alice_id));

    alice_voice.send(VoiceCommand::Members(vec![
        alice_id.clone(),
        bob_id.clone(),
    ]));
    bob_voice.send(VoiceCommand::Members(vec![
        bob_id.clone(),
        alice_id.clone(),
    ]));
    // Muted on join: open both microphones for the exchange.
    alice_voice.send(VoiceCommand::MicMuted(false));
    bob_voice.send(VoiceCommand::MicMuted(false));

    let mut phase_a = 0.0;
    let mut phase_b = 1.0;
    let mut ready = 0;
    let mut alice_pcm = Vec::new();
    let mut bob_pcm = Vec::new();
    for _ in 0..400 {
        let _ = alice_audio.frames.send(sine(&mut phase_a, 0.4));
        let _ = bob_audio.frames.send(sine(&mut phase_b, 0.4));
        for event in std::iter::from_fn(|| alice_voice.try_recv()) {
            if matches!(event, VoiceEvent::Ready) {
                ready += 1;
            }
        }
        for event in std::iter::from_fn(|| bob_voice.try_recv()) {
            if matches!(event, VoiceEvent::Ready) {
                ready += 1;
            }
        }
        alice_pcm.extend(alice_audio.playout.drain());
        bob_pcm.extend(bob_audio.playout.drain());
        if alice_pcm.len() >= FRAME_SAMPLES * 10
            && bob_pcm.len() >= FRAME_SAMPLES * 10
            && ready >= 2
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    assert!(ready >= 2, "both peers report a live connection");
    assert!(
        alice_pcm.len() >= FRAME_SAMPLES * 5,
        "alice received only {} samples",
        alice_pcm.len()
    );
    assert!(
        bob_pcm.len() >= FRAME_SAMPLES * 5,
        "bob received only {} samples",
        bob_pcm.len()
    );
    assert!(rms_f32(&alice_pcm) > 0.1, "alice audio was silent");
    assert!(rms_f32(&bob_pcm) > 0.1, "bob audio was silent");

    alice_voice.send(VoiceCommand::Stop);
    bob_voice.send(VoiceCommand::Stop);
    let _ = alice.disconnect();
    let _ = bob.disconnect();
    drop(alice);
    drop(bob);
    tokio::time::sleep(Duration::from_millis(100)).await;
    server_task.abort();
    let _ = std::fs::remove_dir_all(root("alice"));
    let _ = std::fs::remove_dir_all(root("bob"));
}

fn rms_f32(samples: &[f32]) -> f32 {
    if samples.is_empty() {
        return 0.0;
    }
    (samples.iter().map(|sample| sample * sample).sum::<f32>() / samples.len() as f32).sqrt()
}
