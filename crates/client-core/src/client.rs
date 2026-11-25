//! Client state machine: connect, TOFU trust, SCRAM, rooms, chat, reconnect.

use crate::history::HistoryStore;
use crate::pins::PinStore;
use crate::settings::{ClientSettings, SettingsStore};
use crate::snapshot::{ChatSnapshot, TrustPrompt};
use crate::tls::{self, Socket};
use crate::{ConnectionFailure, ConnectionState, backoff_with_jitter};
use rust_chat_crypto::scram::{
    auth_message, client_proof, derive_auth_key, keys, server_signature,
};
use rust_chat_crypto::{base64_decode, base64_encode, constant_time_eq, random_bytes};
use rust_chat_protocol::payloads::*;
use rust_chat_protocol::{ChatMessage, Envelope, ErrorPayload, PROTOCOL_VERSION, RoomState, kind};
use serde::de::DeserializeOwned;
use std::time::Duration;
use tokio::sync::{mpsc, watch};
use tokio::time::timeout;

const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(15);
const TRUST_TIMEOUT: Duration = Duration::from_secs(120);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnectRequest {
    pub host: String,
    pub port: u16,
    pub tls: bool,
    pub name: String,
    pub password: String,
    pub room: Option<String>,
}

impl ConnectRequest {
    pub fn endpoint(&self) -> String {
        format!("{}:{}", self.host, self.port)
    }
}

#[derive(Debug)]
pub enum Command {
    Connect(ConnectRequest),
    Disconnect,
    Trust(bool),
    Join(String),
    Leave,
    SendChat(String),
    Presence {
        self_mute: Option<bool>,
        self_deaf: Option<bool>,
        sharing: Option<bool>,
        voice: Option<bool>,
    },
    Signal {
        to: String,
        signal: serde_json::Value,
    },
    RefreshRooms,
    ForgetPin {
        host: String,
        port: u16,
    },
    /// Drops the in-memory transcript (the file on disk is cleared by the caller).
    ClearMessages,
}

/// An SDP/ICE payload forwarded by the server. The signal body stays opaque.
#[derive(Debug, Clone)]
pub struct IncomingSignal {
    pub from: String,
    pub signal: serde_json::Value,
}

#[derive(Clone)]
pub struct ClientHandle {
    snapshot: watch::Receiver<ChatSnapshot>,
    commands: mpsc::Sender<Command>,
    signals: tokio::sync::broadcast::Sender<IncomingSignal>,
    keep_history: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

impl ClientHandle {
    pub fn spawn(settings: ClientSettings, runtime: &tokio::runtime::Handle) -> Self {
        Self::spawn_in(settings, runtime, crate::settings::app_dir())
    }

    /// Testable variant: all client state (settings, pins, history) lives under `root`.
    pub fn spawn_in(
        settings: ClientSettings,
        runtime: &tokio::runtime::Handle,
        root: std::path::PathBuf,
    ) -> Self {
        let (commands_tx, commands_rx) = mpsc::channel(64);
        let (snapshot_tx, snapshot_rx) = watch::channel(ChatSnapshot::default());
        let (signal_tx, _) = tokio::sync::broadcast::channel(64);
        let keep_history =
            std::sync::Arc::new(std::sync::atomic::AtomicBool::new(settings.keep_history));
        let task = ClientTask {
            settings,
            snapshot_tx,
            commands: commands_rx,
            signals: signal_tx.clone(),
            pins: PinStore::at(root.join("pins.json")),
            history: HistoryStore::at(root.clone()),
            settings_store: SettingsStore::at(root.join("settings.json")),
            keep_history: keep_history.clone(),
        };
        runtime.spawn(task.run());
        Self {
            snapshot: snapshot_rx,
            commands: commands_tx,
            signals: signal_tx,
            keep_history,
        }
    }

    /// Applies a settings change that the client honours for the next message instead of
    /// waiting for a reconnect.
    pub fn set_keep_history(&self, keep: bool) {
        self.keep_history
            .store(keep, std::sync::atomic::Ordering::Relaxed);
    }

    pub fn snapshot(&self) -> ChatSnapshot {
        self.snapshot.borrow().clone()
    }

    /// Subscribes to forwarded WebRTC signals (offer/answer/ICE). Every call gets a
    /// fresh receiver, so leaving and rejoining a voice room works.
    pub fn subscribe_signals(&self) -> tokio::sync::broadcast::Receiver<IncomingSignal> {
        self.signals.subscribe()
    }

    pub fn connect(&self, request: ConnectRequest) -> bool {
        self.send(Command::Connect(request))
    }

    pub fn disconnect(&self) -> bool {
        self.send(Command::Disconnect)
    }

    pub fn trust(&self, approved: bool) -> bool {
        self.send(Command::Trust(approved))
    }

    pub fn join(&self, room_id: impl Into<String>) -> bool {
        self.send(Command::Join(room_id.into()))
    }

    pub fn leave(&self) -> bool {
        self.send(Command::Leave)
    }

    pub fn send_chat(&self, text: impl Into<String>) -> bool {
        self.send(Command::SendChat(text.into()))
    }

    pub fn set_presence(
        &self,
        self_mute: Option<bool>,
        self_deaf: Option<bool>,
        sharing: Option<bool>,
    ) -> bool {
        self.send(Command::Presence {
            self_mute,
            self_deaf,
            sharing,
            voice: None,
        })
    }

    /// Announces that this client is (or is no longer) in the room's voice mesh.
    pub fn set_voice(&self, voice: bool) -> bool {
        self.send(Command::Presence {
            self_mute: None,
            self_deaf: None,
            sharing: None,
            voice: Some(voice),
        })
    }

    /// Sends one opaque offer/answer/ICE body to a member of the current room.
    pub fn send_signal(&self, to: impl Into<String>, signal: serde_json::Value) -> bool {
        self.send(Command::Signal {
            to: to.into(),
            signal,
        })
    }

    pub fn refresh_rooms(&self) -> bool {
        self.send(Command::RefreshRooms)
    }

    pub fn forget_pin(&self, host: impl Into<String>, port: u16) -> bool {
        self.send(Command::ForgetPin {
            host: host.into(),
            port,
        })
    }

    /// Clears the transcript shown in the room panel. The history file is removed separately.
    pub fn clear_messages(&self) -> bool {
        self.send(Command::ClearMessages)
    }

    fn send(&self, command: Command) -> bool {
        self.commands.try_send(command).is_ok()
    }
}

struct ClientTask {
    settings: ClientSettings,
    snapshot_tx: watch::Sender<ChatSnapshot>,
    commands: mpsc::Receiver<Command>,
    signals: tokio::sync::broadcast::Sender<IncomingSignal>,
    pins: PinStore,
    history: HistoryStore,
    settings_store: SettingsStore,
    keep_history: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

struct Session {
    socket: Socket,
}

enum End {
    Disconnect,
    Fatal(String),
    Reconnect(String),
}

enum SessionError {
    Fatal(String),
    Transient(String),
    Server(ErrorPayload),
}

impl std::fmt::Display for SessionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Fatal(message) | Self::Transient(message) => f.write_str(message),
            Self::Server(error) => write!(f, "{}: {}", error.code, error.message),
        }
    }
}

impl ClientTask {
    async fn run(mut self) {
        loop {
            match self.commands.recv().await {
                Some(Command::Connect(request)) => self.run_connection(request).await,
                Some(Command::ForgetPin { host, port }) => {
                    if let Err(error) = self.pins.forget(&host, port) {
                        self.set_error(format!("cannot forget the pinned key: {error}"));
                    } else {
                        self.update(|snapshot| snapshot.server_fingerprint = None);
                    }
                }
                Some(Command::ClearMessages) => self.update(|snapshot| snapshot.messages.clear()),
                Some(Command::Disconnect) | None => {
                    self.set_idle();
                    return;
                }
                _ => {}
            }
        }
    }

    #[allow(unused_assignments)]
    async fn run_connection(&mut self, mut request: ConnectRequest) {
        let mut attempt = 0u32;
        let mut cause: Option<String> = None;
        loop {
            if attempt == 0 {
                self.update(|snapshot| {
                    snapshot.last_error = None;
                    snapshot.connection = ConnectionState::Connecting {
                        endpoint: request.endpoint(),
                    };
                });
            }
            match self.open_session(&request).await {
                Ok(session) => {
                    attempt = 0;
                    match self.session_loop(session, &request).await {
                        End::Disconnect => {
                            self.set_idle();
                            return;
                        }
                        End::Fatal(message) => {
                            self.set_failed(message);
                            return;
                        }
                        End::Reconnect(reason) => cause = Some(reason),
                    }
                }
                Err(SessionError::Fatal(message)) => {
                    self.set_failed(message);
                    return;
                }
                Err(SessionError::Transient(message)) => cause = Some(message),
                Err(SessionError::Server(error)) => {
                    self.set_failed(error.message);
                    return;
                }
            }

            attempt = attempt.saturating_add(1);
            let jitter = 0.8 + rand::random::<f32>() * 0.4;
            let delay = backoff_with_jitter(attempt, jitter);
            self.update(|snapshot| {
                snapshot.connection = ConnectionState::Reconnecting {
                    attempt,
                    delay,
                    cause: cause.clone(),
                    hint: hint_for(cause.as_deref()),
                };
            });
            tokio::select! {
                _ = tokio::time::sleep(delay) => {}
                command = self.commands.recv() => match command {
                    Some(Command::Disconnect) | None => {
                        self.set_idle();
                        return;
                    }
                    Some(Command::Connect(next)) => {
                        request = next;
                        attempt = 0;
                        cause = None;
                    }
                    Some(Command::ForgetPin { host, port }) => {
                        let _ = self.pins.forget(&host, port);
                    }
                    _ => {}
                }
            }
        }
    }

    async fn open_session(&mut self, request: &ConnectRequest) -> Result<Session, SessionError> {
        self.update(|snapshot| {
            snapshot.last_error = None;
            snapshot.trust = None;
            snapshot.connection = ConnectionState::Connecting {
                endpoint: request.endpoint(),
            };
        });

        let pin = if request.tls {
            self.pins
                .get(&request.host, request.port)
                .map(str::to_owned)
        } else {
            None
        };
        let (mut socket, captured) =
            tls::connect(&request.host, request.port, request.tls, pin.clone())
                .await
                .map_err(|error| {
                    let changed = pin
                        .as_ref()
                        .zip(error.captured.as_ref())
                        .is_some_and(|(pinned, actual)| pinned != actual);
                    if changed {
                        SessionError::Fatal(error.message)
                    } else {
                        SessionError::Transient(error.message)
                    }
                })?;

        if request.tls {
            match (&pin, &captured) {
                (None, Some(actual)) => {
                    self.update(|snapshot| {
                        snapshot.trust = Some(TrustPrompt {
                            fingerprint: actual.clone(),
                        });
                    });
                    if !self.await_trust().await? {
                        tls::close(&mut socket).await;
                        return Err(SessionError::Fatal(
                            "the server key was not approved".into(),
                        ));
                    }
                    if let Err(error) = self.pins.set(&request.host, request.port, actual) {
                        tls::close(&mut socket).await;
                        return Err(SessionError::Fatal(format!(
                            "cannot save the pinned key: {error}"
                        )));
                    }
                    self.update(|snapshot| {
                        snapshot.trust = None;
                        snapshot.server_fingerprint = Some(actual.clone());
                    });
                }
                (Some(expected), Some(actual)) if expected == actual => {
                    self.update(|snapshot| {
                        snapshot.server_fingerprint = Some(actual.clone());
                    });
                }
                _ => {}
            }
        }

        self.update(|snapshot| {
            snapshot.connection = ConnectionState::Authenticating;
        });
        if let Err(error) = self.handshake(&mut socket, request).await {
            tls::close(&mut socket).await;
            return Err(error);
        }
        self.update(|snapshot| {
            snapshot.connection = ConnectionState::Ready;
        });
        Ok(Session { socket })
    }

    async fn await_trust(&mut self) -> Result<bool, SessionError> {
        let deadline = tokio::time::Instant::now() + TRUST_TIMEOUT;
        loop {
            tokio::select! {
                _ = tokio::time::sleep_until(deadline) => {
                    return Err(SessionError::Fatal("the server key prompt timed out".into()));
                }
                command = self.commands.recv() => match command {
                    Some(Command::Trust(approved)) => return Ok(approved),
                    Some(Command::Disconnect) | None => {
                        return Err(SessionError::Fatal("disconnected before the key was approved".into()));
                    }
                    _ => {}
                }
            }
        }
    }

    async fn handshake(
        &mut self,
        socket: &mut Socket,
        request: &ConnectRequest,
    ) -> Result<(), SessionError> {
        let identity = if self.settings.identity.is_empty() {
            let value = base64_encode(&random_bytes(32));
            self.settings.identity = value.clone();
            let _ = self.settings_store.save(&self.settings);
            value
        } else {
            self.settings.identity.clone()
        };

        send(
            socket,
            Envelope::new(
                kind::HELLO,
                serde_json::to_value(Hello {
                    protocol: PROTOCOL_VERSION,
                    name: request.name.clone(),
                    pubkey: identity,
                    features: Vec::new(),
                })
                .map_err(|error| SessionError::Fatal(error.to_string()))?,
            ),
        )
        .await?;

        let challenge: Challenge = recv_payload(socket, kind::CHALLENGE).await?;
        let server_nonce = base64_decode(&challenge.nonce)
            .map_err(|error| SessionError::Fatal(format!("invalid server nonce: {error}")))?;
        let client_nonce = random_bytes(16);
        let password = request.password.clone();
        let kdf = challenge.kdf.clone();
        let name = request.name.clone();
        let auth_key = tokio::task::spawn_blocking(move || derive_auth_key(&password, &kdf))
            .await
            .map_err(|error| SessionError::Transient(error.to_string()))?
            .map_err(|error| SessionError::Fatal(format!("key derivation failed: {error}")))?;
        let keys = keys(&auth_key);
        let message = auth_message(&name, &server_nonce, &client_nonce);
        let proof = client_proof(&keys, &message);
        let expected_server_signature = server_signature(&keys.server_key, &message);
        send(
            socket,
            Envelope::new(
                kind::AUTH,
                serde_json::to_value(Auth {
                    client_nonce: base64_encode(&client_nonce),
                    proof: base64_encode(&proof),
                })
                .map_err(|error| SessionError::Fatal(error.to_string()))?,
            ),
        )
        .await?;

        let welcome: Welcome = recv_payload(socket, kind::WELCOME).await?;
        let received_signature = base64_decode(&welcome.server_signature)
            .map_err(|error| SessionError::Fatal(format!("invalid server signature: {error}")))?;
        if !constant_time_eq(&received_signature, &expected_server_signature) {
            return Err(SessionError::Fatal(
                "the server could not prove the shared password".into(),
            ));
        }
        if welcome.protocol != PROTOCOL_VERSION {
            return Err(SessionError::Fatal(format!(
                "server speaks protocol {}, expected {}",
                welcome.protocol, PROTOCOL_VERSION
            )));
        }

        let rooms = welcome.rooms.clone();
        let server_name = welcome.server_name.clone();
        let motd = welcome.motd.clone();
        let self_id = welcome.session_id.clone();
        let max_chat_bytes = welcome.limits.max_chat_bytes;
        let turn = welcome.turn.clone();
        self.update(|snapshot| {
            snapshot.server_name = server_name;
            snapshot.motd = motd;
            snapshot.self_id = Some(self_id);
            snapshot.rooms = rooms;
            snapshot.max_chat_bytes = max_chat_bytes;
            snapshot.last_error = None;
            snapshot.turn = turn;
        });

        let state: RoomState = recv_payload(socket, kind::ROOM_STATE).await?;
        let current_room = state.room.id.clone();
        self.apply_room_state(state, request);

        let wanted = request
            .room
            .clone()
            .or_else(|| self.settings.last_room_id.clone());
        if let Some(wanted) = wanted
            && wanted != current_room
        {
            send(
                socket,
                Envelope::new(
                    kind::ROOM_JOIN,
                    serde_json::to_value(RoomJoin { room_id: wanted }).unwrap(),
                ),
            )
            .await?;
            match recv_payload::<RoomState>(socket, kind::ROOM_STATE).await {
                Ok(state) => self.apply_room_state(state, request),
                Err(SessionError::Server(error))
                    if error.code == rust_chat_protocol::ErrorCode::RoomNotFound =>
                {
                    self.set_error(format!("the last room no longer exists: {}", error.message));
                }
                Err(error) => return Err(error),
            }
        }
        Ok(())
    }

    async fn session_loop(&mut self, mut session: Session, request: &ConnectRequest) -> End {
        let mut heartbeat = tokio::time::interval(Duration::from_secs(20));
        heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                command = self.commands.recv() => match command {
                    Some(Command::Disconnect) | None => {
                        tls::close(&mut session.socket).await;
                        return End::Disconnect;
                    }
                    Some(Command::SendChat(text)) => {
                        if let Err(end) = self.send_chat(&mut session.socket, text).await {
                            return end;
                        }
                    }
                    Some(Command::Join(room_id)) => {
                        if let Err(error) = send(
                            &mut session.socket,
                            Envelope::new(
                                kind::ROOM_JOIN,
                                serde_json::to_value(RoomJoin { room_id }).unwrap(),
                            ),
                        ).await {
                            return End::Reconnect(error.to_string());
                        }
                    }
                    Some(Command::Leave) => {
                        if let Err(error) = send(
                            &mut session.socket,
                            Envelope::new(kind::ROOM_LEAVE, serde_json::json!({})),
                        ).await {
                            return End::Reconnect(error.to_string());
                        }
                    }
                    Some(Command::Presence { self_mute, self_deaf, sharing, voice }) => {
                        if let Err(error) = send(
                            &mut session.socket,
                            Envelope::new(
                                kind::PRESENCE_UPDATE,
                                serde_json::to_value(PresenceUpdate { self_mute, self_deaf, sharing, voice }).unwrap(),
                            ),
                        ).await {
                            return End::Reconnect(error.to_string());
                        }
                    }
                    Some(Command::Signal { to, signal }) => {
                        if let Err(error) = send(
                            &mut session.socket,
                            Envelope::new(
                                kind::RTC_SIGNAL,
                                serde_json::to_value(RtcSignal { to, signal }).unwrap(),
                            ),
                        ).await {
                            return End::Reconnect(error.to_string());
                        }
                    }
                    Some(Command::RefreshRooms) => {
                        if let Err(error) = send(
                            &mut session.socket,
                            Envelope::new(kind::ROOM_LIST, serde_json::json!({})),
                        ).await {
                            return End::Reconnect(error.to_string());
                        }
                    }
                    Some(Command::ForgetPin { host, port }) => {
                        let _ = self.pins.forget(&host, port);
                    }
                    Some(Command::ClearMessages) => {
                        self.update(|snapshot| snapshot.messages.clear());
                    }
                    Some(Command::Trust(_)) | Some(Command::Connect(_)) => {}
                },
                message = tls::next_envelope(&mut session.socket) => match message {
                    None => return End::Reconnect("the connection closed".into()),
                    Some(Err(error)) => return End::Reconnect(error),
                    Some(Ok(envelope)) => {
                        if let Some(end) = self.handle_envelope(envelope, request) {
                            return end;
                        }
                    }
                },
                _ = heartbeat.tick() => {
                    let ts = crate::now_ms();
                    if let Err(error) = send(
                        &mut session.socket,
                        Envelope::new(kind::PING, serde_json::json!({ "ts": ts })),
                    ).await {
                        return End::Reconnect(error.to_string());
                    }
                }
            }
        }
    }

    async fn send_chat(&mut self, socket: &mut Socket, text: String) -> Result<(), End> {
        if self.snapshot_tx.borrow().room.is_none() {
            self.set_error("join a room before sending a message".into());
            return Ok(());
        }
        let text = text.trim().to_owned();
        if text.is_empty() {
            return Ok(());
        }
        let client_msg_id = format!("c-{:016x}", rand::random::<u64>());
        send(
            socket,
            Envelope::new(
                kind::CHAT_SEND,
                serde_json::json!({ "text": text, "clientMsgId": client_msg_id }),
            ),
        )
        .await
        .map_err(|error| End::Reconnect(error.to_string()))
    }

    fn handle_envelope(&mut self, envelope: Envelope, request: &ConnectRequest) -> Option<End> {
        match envelope.kind.as_str() {
            kind::ROOM_STATE => {
                let state: RoomState = serde_json::from_value(envelope.payload).ok()?;
                self.apply_room_state(state, request);
                None
            }
            kind::CHAT_MESSAGE => {
                let message: ChatMessage = serde_json::from_value(envelope.payload).ok()?;
                self.append_message(message, request);
                None
            }
            kind::ROOM_LEFT => {
                let rooms: RoomsPayload = serde_json::from_value(envelope.payload).ok()?;
                self.update(|snapshot| {
                    snapshot.room = None;
                    snapshot.roomless = true;
                    snapshot.messages.clear();
                    snapshot.rooms = rooms.rooms;
                });
                None
            }
            kind::ROOM_LIST_RESULT => {
                let rooms: RoomsPayload = serde_json::from_value(envelope.payload).ok()?;
                self.update(|snapshot| snapshot.rooms = rooms.rooms);
                None
            }
            kind::ROOM_DELETED => {
                let room_id = envelope.payload.get("roomId")?.as_str()?.to_owned();
                self.update(|snapshot| snapshot.rooms.retain(|room| room.id != room_id));
                None
            }
            kind::ROOM_FLAGS => {
                let flags: RoomFlags = serde_json::from_value(envelope.payload).ok()?;
                self.update(|snapshot| snapshot.deletable_rooms = flags.deletable);
                None
            }
            kind::MEMBER_JOINED => {
                let joined: MemberJoined = serde_json::from_value(envelope.payload).ok()?;
                self.update(|snapshot| {
                    if let Some(room) = snapshot.room.as_mut()
                        && !room
                            .members
                            .iter()
                            .any(|member| member.session_id == joined.member.session_id)
                    {
                        room.members.push(joined.member);
                    }
                });
                None
            }
            kind::MEMBER_LEFT => {
                let left: MemberLeft = serde_json::from_value(envelope.payload).ok()?;
                self.update(|snapshot| {
                    if let Some(room) = snapshot.room.as_mut() {
                        room.members
                            .retain(|member| member.session_id != left.session_id);
                    }
                });
                None
            }
            kind::MEMBER_UPDATED => {
                let update: MemberUpdated = serde_json::from_value(envelope.payload).ok()?;
                self.update(|snapshot| {
                    if let Some(room) = snapshot.room.as_mut()
                        && let Some(member) = room
                            .members
                            .iter_mut()
                            .find(|member| member.session_id == update.session_id)
                    {
                        member.self_mute = update.self_mute;
                        member.self_deaf = update.self_deaf;
                        member.sharing = update.sharing;
                        member.voice = update.voice;
                    }
                });
                None
            }
            kind::RTC_SIGNAL => {
                let forward: RtcSignalForward = serde_json::from_value(envelope.payload).ok()?;
                let _ = self.signals.send(IncomingSignal {
                    from: forward.from,
                    signal: forward.signal,
                });
                None
            }
            kind::PONG => {
                let pong: Pong = serde_json::from_value(envelope.payload).ok()?;
                let latency = crate::now_ms().saturating_sub(pong.ts);
                self.update(|snapshot| snapshot.latency_ms = Some(latency));
                None
            }
            kind::ERROR => {
                let error: ErrorPayload = serde_json::from_value(envelope.payload).ok()?;
                self.set_error(error.message.clone());
                if error.fatal {
                    Some(End::Fatal(error.message))
                } else {
                    None
                }
            }
            kind::SERVER_SHUTDOWN => {
                let reason = envelope
                    .payload
                    .get("reason")
                    .and_then(|value| value.as_str())
                    .unwrap_or("the server is shutting down")
                    .to_owned();
                self.set_error(reason.clone());
                Some(End::Reconnect(reason))
            }
            _ => None,
        }
    }

    fn apply_room_state(&mut self, state: RoomState, request: &ConnectRequest) {
        let room_id = state.room.id.clone();
        let previous_room = self.snapshot_tx.borrow().room_id().map(str::to_owned);
        let changed = previous_room.as_deref() != Some(room_id.as_str());
        let messages = if changed {
            self.history.load(&request.endpoint(), &room_id)
        } else {
            self.snapshot_tx.borrow().messages.clone()
        };
        self.settings.last_room_id = Some(room_id.clone());
        self.settings.name = state.self_member.name.clone();
        let _ = self.settings_store.save(&self.settings);
        self.update(|snapshot| {
            snapshot.room = Some(state);
            snapshot.roomless = false;
            if changed {
                snapshot.messages = messages;
            }
        });
    }

    fn append_message(&mut self, message: ChatMessage, request: &ConnectRequest) {
        if self.snapshot_tx.borrow().room_id() != Some(message.room_id.as_str()) {
            return;
        }
        if self
            .snapshot_tx
            .borrow()
            .messages
            .iter()
            .any(|known| known.message_id == message.message_id)
        {
            return;
        }
        if self.keep_history.load(std::sync::atomic::Ordering::Relaxed) {
            let _ = self
                .history
                .append(&request.endpoint(), &message.room_id, &message);
        }
        self.update(|snapshot| {
            snapshot.messages.push(message);
            if snapshot.messages.len() > crate::history::WINDOW {
                let excess = snapshot.messages.len() - crate::history::WINDOW;
                snapshot.messages.drain(..excess);
            }
        });
    }

    fn update(&self, f: impl FnOnce(&mut ChatSnapshot)) {
        self.snapshot_tx.send_modify(f);
    }

    fn set_idle(&self) {
        self.update(|snapshot| {
            snapshot.connection = ConnectionState::Idle;
            snapshot.trust = None;
            snapshot.turn = None;
        });
    }

    fn set_failed(&self, message: String) {
        let hint = hint_for(Some(&message));
        self.update(|snapshot| {
            snapshot.last_error = Some(message.clone());
            snapshot.trust = None;
            snapshot.connection =
                ConnectionState::Failed(ConnectionFailure::new(message).with_optional_hint(hint));
        });
    }

    fn set_error(&self, message: String) {
        self.update(|snapshot| snapshot.last_error = Some(message));
    }
}

#[derive(serde::Deserialize)]
struct RoomsPayload {
    rooms: Vec<rust_chat_protocol::RoomSummary>,
}

#[derive(serde::Deserialize)]
struct MemberJoined {
    member: rust_chat_protocol::Member,
}

async fn send(socket: &mut Socket, envelope: Envelope) -> Result<(), SessionError> {
    tls::send_envelope(socket, &envelope)
        .await
        .map_err(|error| SessionError::Transient(error.to_string()))
}

async fn recv(socket: &mut Socket, wait: Duration) -> Result<Envelope, SessionError> {
    match timeout(wait, tls::next_envelope(socket)).await {
        Ok(Some(Ok(envelope))) => {
            if envelope.kind == kind::ERROR {
                let error: ErrorPayload = serde_json::from_value(envelope.payload)
                    .map_err(|error| SessionError::Fatal(error.to_string()))?;
                Err(SessionError::Server(error))
            } else {
                Ok(envelope)
            }
        }
        Ok(Some(Err(error))) => Err(SessionError::Transient(error)),
        Ok(None) => Err(SessionError::Transient("the connection closed".into())),
        Err(_) => Err(SessionError::Transient("the server did not answer".into())),
    }
}

async fn recv_payload<T: DeserializeOwned>(
    socket: &mut Socket,
    expected: &str,
) -> Result<T, SessionError> {
    let envelope = recv(socket, HANDSHAKE_TIMEOUT).await?;
    if envelope.kind != expected {
        return Err(SessionError::Fatal(format!(
            "expected `{expected}`, got `{}`",
            envelope.kind
        )));
    }
    serde_json::from_value(envelope.payload)
        .map_err(|error| SessionError::Fatal(format!("invalid `{expected}` payload: {error}")))
}

fn hint_for(message: Option<&str>) -> Option<String> {
    let message = message?.to_ascii_lowercase();
    if message.contains("certificate") || message.contains("pinned") {
        Some("If the server was reinstalled, forget its key and approve the new one.".into())
    } else if message.contains("password") || message.contains("auth") {
        Some("Check the server password.".into())
    } else if message.contains("connect") || message.contains("timed out") {
        Some("Check the address, port and that the server is running.".into())
    } else {
        None
    }
}
