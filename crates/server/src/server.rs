//! Control-plane server: WebSocket lifecycle, handshake, rooms, chat and signaling.

use crate::auth::{self, AuthFailure};
use crate::state::{Session, Shared, State, now_ms, payload_envelope};
use crate::turn;
use axum::Router;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{ConnectInfo, State as AxumState};
use axum::response::IntoResponse;
use axum::routing::get;
use futures_util::{SinkExt, StreamExt};
use rust_chat_protocol::payloads::*;
use rust_chat_protocol::{ChatMessage, Envelope, ErrorCode, ErrorPayload, PROTOCOL_VERSION, kind};
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{Notify, mpsc};

const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(15);
const HEARTBEAT: Duration = Duration::from_secs(20);

pub fn router(shared: Arc<Shared>) -> Router {
    Router::new()
        .route("/", get(ws_handler))
        .route("/healthz", get(|| async { "ok" }))
        .with_state(shared)
}

async fn ws_handler(
    ws: WebSocketUpgrade,
    ConnectInfo(address): ConnectInfo<SocketAddr>,
    AxumState(shared): AxumState<Arc<Shared>>,
) -> impl IntoResponse {
    ws.max_frame_size(rust_chat_protocol::MAX_FRAME_BYTES)
        .max_message_size(rust_chat_protocol::MAX_FRAME_BYTES)
        .on_upgrade(move |socket| connection(socket, shared, address.ip()))
}

async fn connection(socket: WebSocket, shared: Arc<Shared>, ip: IpAddr) {
    let (sink, mut stream) = socket.split();
    let (tx, rx) = mpsc::channel(crate::state::SESSION_QUEUE);
    let close = Arc::new(Notify::new());
    let writer = tokio::spawn(writer_loop(sink, rx, close.clone()));

    let session_id = handshake(&mut stream, &tx, &shared, ip, close.clone()).await;
    if let Some(session_id) = &session_id {
        reader_loop(&mut stream, &shared, session_id, close.clone()).await;
        shared
            .lock_state()
            .disconnect(&shared.config, session_id, "disconnect");
    }
    close.notify_one();
    let _ = writer.await;
}

async fn writer_loop(
    mut sink: futures_util::stream::SplitSink<WebSocket, Message>,
    mut rx: mpsc::Receiver<Envelope>,
    close: Arc<Notify>,
) {
    let mut heartbeat = tokio::time::interval(HEARTBEAT);
    heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            _ = close.notified() => {
                // Flush what was queued (an auth error, a final member.left) before closing.
                while let Ok(envelope) = rx.try_recv() {
                    if let Ok(bytes) = envelope.encode()
                        && let Ok(text) = String::from_utf8(bytes)
                        && sink.send(Message::Text(text.into())).await.is_err()
                    {
                        break;
                    }
                }
                let _ = sink.send(Message::Close(None)).await;
                break;
            }
            _ = heartbeat.tick() => {
                if sink.send(Message::Ping(axum::body::Bytes::new())).await.is_err() {
                    break;
                }
            }
            message = rx.recv() => {
                let Some(envelope) = message else {
                    let _ = sink.send(Message::Close(None)).await;
                    break;
                };
                let text = match envelope.encode() {
                    Ok(bytes) => match String::from_utf8(bytes) {
                        Ok(text) => text,
                        Err(error) => {
                            tracing::error!("server produced invalid UTF-8: {error}");
                            continue;
                        }
                    },
                    Err(error) => {
                        tracing::error!("cannot encode an envelope: {error}");
                        continue;
                    }
                };
                if sink.send(Message::Text(text.into())).await.is_err() {
                    break;
                }
            }
        }
    }
}

async fn handshake(
    stream: &mut futures_util::stream::SplitStream<WebSocket>,
    tx: &mpsc::Sender<Envelope>,
    shared: &Arc<Shared>,
    ip: IpAddr,
    close: Arc<Notify>,
) -> Option<String> {
    if !shared.auth_allow(ip) {
        send_error(
            tx,
            ErrorCode::AuthRateLimit,
            "too many authentication attempts from this address",
            false,
            true,
        );
        return None;
    }

    let hello: Hello = match next_payload(stream, kind::HELLO).await {
        Ok(value) => value,
        Err(error) => {
            send_failure(tx, error);
            return None;
        }
    };

    if hello.protocol != PROTOCOL_VERSION {
        send_error(
            tx,
            ErrorCode::ProtoVersion,
            format!("this server speaks protocol {PROTOCOL_VERSION}"),
            false,
            true,
        );
        return None;
    }

    let name = match auth::validate_name(&hello.name) {
        Ok(name) => name,
        Err(error) => {
            send_failure(tx, error);
            return None;
        }
    };
    let pubkey_fp = match auth::pubkey_fingerprint(&hello.pubkey) {
        Ok(fingerprint) => fingerprint,
        Err(error) => {
            send_failure(tx, error);
            return None;
        }
    };

    let (challenge, server_nonce) = auth::challenge(&shared.verifier.kdf);
    direct(tx, payload_envelope(kind::CHALLENGE, &challenge));

    let proof: Auth = match next_payload(stream, kind::AUTH).await {
        Ok(value) => value,
        Err(error) => {
            send_failure(tx, error);
            return None;
        }
    };

    let server_signature = match auth::verify(&shared.verifier, &name, &server_nonce, &proof) {
        Ok(signature) => signature,
        Err(error) => {
            shared.auth_fail(ip);
            send_failure(tx, error);
            return None;
        }
    };
    shared.auth_success(ip);

    let session_id = format!("s_{:016x}", rand::random::<u64>());
    let mut state = shared.lock_state();
    let name = unique_name(&state, &name);
    let session = Session::new(
        session_id.clone(),
        name,
        pubkey_fp,
        hello.features.clone(),
        tx.clone(),
        close,
    );
    let roomless = hello
        .features
        .iter()
        .any(|feature| feature == "no-auto-room");
    state.sessions.insert(session_id.clone(), session);
    let rooms = state.room_summaries(&shared.config);
    let turn = shared
        .config
        .turn
        .as_ref()
        .map(|config| turn::credentials(config, &session_id));
    let welcome = Welcome {
        session_id: session_id.clone(),
        server_name: shared.config.server_name.clone(),
        motd: shared.config.motd.clone(),
        protocol: PROTOCOL_VERSION,
        server_signature: rust_chat_crypto::base64_encode(&server_signature),
        rooms,
        turn,
        limits: Limits {
            max_chat_bytes: shared.config.max_chat_bytes,
            max_members_per_room: shared.config.max_users_per_room,
        },
    };
    let _ = state.send(&session_id, payload_envelope(kind::WELCOME, &welcome));
    if !roomless {
        let room = state.default_room_id();
        if let Err((code, message)) = state.join_room(&shared.config, &session_id, &room) {
            let _ = state.send(
                &session_id,
                payload_envelope(
                    kind::ERROR,
                    &ErrorPayload {
                        code,
                        message,
                        retryable: false,
                        fatal: false,
                    },
                ),
            );
        }
    }
    drop(state);
    Some(session_id)
}

async fn reader_loop(
    stream: &mut futures_util::stream::SplitStream<WebSocket>,
    shared: &Arc<Shared>,
    session_id: &str,
    close: Arc<Notify>,
) {
    loop {
        tokio::select! {
            _ = close.notified() => break,
            message = stream.next() => {
                match message {
                    Some(Ok(Message::Text(text))) => {
                        let envelope = match Envelope::decode(text.as_bytes()) {
                            Ok(envelope) => envelope,
                            Err(_) => {
                                send_error_to(shared, session_id, ErrorCode::BadPayload, "invalid frame", false, false);
                                continue;
                            }
                        };
                        if !dispatch(shared, session_id, envelope) {
                            break;
                        }
                    }
                    Some(Ok(Message::Close(_))) | None => break,
                    Some(Ok(_)) => {}
                    Some(Err(_)) => break,
                }
            }
        }
    }
}

fn dispatch(shared: &Arc<Shared>, session_id: &str, envelope: Envelope) -> bool {
    if !shared.lock_state().sessions.contains_key(session_id) {
        return false;
    }

    match envelope.kind.as_str() {
        kind::ROOM_LIST => {
            let rooms = shared.lock_state().room_summaries(&shared.config);
            send_payload(
                shared,
                session_id,
                kind::ROOM_LIST_RESULT,
                &serde_json::json!({ "rooms": rooms }),
            );
        }
        kind::ROOM_JOIN => match parse::<RoomJoin>(&envelope) {
            Ok(request) => {
                let result =
                    shared
                        .lock_state()
                        .join_room(&shared.config, session_id, &request.room_id);
                if let Err((code, message)) = result {
                    send_error_to(shared, session_id, code, &message, false, false);
                }
            }
            Err(error) => send_failure_to(shared, session_id, error),
        },
        kind::ROOM_LEAVE => {
            shared.lock_state().leave_room(&shared.config, session_id);
        }
        kind::ROOM_CREATE => match parse::<RoomCreate>(&envelope) {
            Ok(request) => {
                let result = shared.lock_state().create_room(
                    &shared.config,
                    session_id,
                    &request.name,
                    &request.topic,
                    request.voice,
                );
                match result {
                    Ok(rooms) => {
                        if let Err(error) = shared.save_client_rooms(&rooms) {
                            tracing::error!("cannot save client rooms: {error}");
                        }
                    }
                    Err((code, message)) => {
                        send_error_to(shared, session_id, code, &message, false, false)
                    }
                }
            }
            Err(error) => send_failure_to(shared, session_id, error),
        },
        kind::ROOM_DELETE => match parse::<RoomDelete>(&envelope) {
            Ok(request) => {
                let result = shared
                    .lock_state()
                    .delete_room(&shared.config, &request.room_id);
                match result {
                    Ok(rooms) => {
                        if let Err(error) = shared.save_client_rooms(&rooms) {
                            tracing::error!("cannot save client rooms: {error}");
                        }
                    }
                    Err((code, message)) => {
                        send_error_to(shared, session_id, code, &message, false, false)
                    }
                }
            }
            Err(error) => send_failure_to(shared, session_id, error),
        },
        kind::CHAT_SEND => match parse::<ChatSend>(&envelope) {
            Ok(request) => handle_chat(shared, session_id, request),
            Err(error) => send_failure_to(shared, session_id, error),
        },
        kind::PRESENCE_UPDATE => match parse::<PresenceUpdate>(&envelope) {
            Ok(request) => handle_presence(shared, session_id, request),
            Err(error) => send_failure_to(shared, session_id, error),
        },
        kind::RTC_SIGNAL => match parse::<RtcSignal>(&envelope) {
            Ok(request) => handle_signal(shared, session_id, request),
            Err(error) => send_failure_to(shared, session_id, error),
        },
        kind::FILE_OFFER => match parse::<FileOffer>(&envelope) {
            Ok(request) => handle_file_offer(shared, session_id, request),
            Err(error) => send_failure_to(shared, session_id, error),
        },
        kind::FILE_ACCEPT => match parse::<FileAccept>(&envelope) {
            Ok(request) => {
                relay_file(
                    shared,
                    session_id,
                    &request.to,
                    kind::FILE_ACCEPT,
                    serde_json::json!({
                        "from": session_id,
                        "transferId": request.transfer_id,
                    }),
                );
            }
            Err(error) => send_failure_to(shared, session_id, error),
        },
        kind::FILE_DECLINE => match parse::<FileDecline>(&envelope) {
            Ok(request) => {
                relay_file(
                    shared,
                    session_id,
                    &request.to,
                    kind::FILE_DECLINE,
                    serde_json::json!({
                        "from": session_id,
                        "transferId": request.transfer_id,
                        "reason": request.reason,
                    }),
                );
            }
            Err(error) => send_failure_to(shared, session_id, error),
        },
        kind::FILE_CANCEL => match parse::<FileCancel>(&envelope) {
            Ok(request) => match request.to {
                Some(to) => relay_file(
                    shared,
                    session_id,
                    &to,
                    kind::FILE_CANCEL,
                    serde_json::json!({
                        "from": session_id,
                        "transferId": request.transfer_id,
                    }),
                ),
                None => {
                    let state = shared.lock_state();
                    if let Some(room) = current_room(&state, session_id) {
                        let payload = serde_json::json!({
                            "from": session_id,
                            "transferId": request.transfer_id,
                        });
                        state.broadcast_room(
                            &room,
                            payload_envelope(kind::FILE_CANCEL, &payload),
                            None,
                        );
                    } else {
                        drop(state);
                        send_error_to(
                            shared,
                            session_id,
                            ErrorCode::NotInRoom,
                            "join a room first",
                            false,
                            false,
                        );
                    }
                }
            },
            Err(error) => send_failure_to(shared, session_id, error),
        },
        kind::PING => {
            let ts = envelope
                .payload
                .get("ts")
                .and_then(serde_json::Value::as_i64)
                .unwrap_or(0);
            send_payload(
                shared,
                session_id,
                kind::PONG,
                &Pong {
                    ts,
                    server_ts: now_ms(),
                },
            );
        }
        _ => {}
    }
    true
}

fn handle_chat(shared: &Arc<Shared>, session_id: &str, request: ChatSend) {
    let text = request.text.trim();
    if text.is_empty() {
        send_error_to(
            shared,
            session_id,
            ErrorCode::BadPayload,
            "message is empty",
            false,
            false,
        );
        return;
    }
    if text.len() > shared.config.max_chat_bytes {
        send_error_to(
            shared,
            session_id,
            ErrorCode::PayloadTooLarge,
            "message is too long",
            false,
            false,
        );
        return;
    }
    let mut state = shared.lock_state();
    let Some(room) = current_room(&state, session_id) else {
        send_error_locked(
            &state,
            session_id,
            ErrorCode::NotInRoom,
            "join a room first",
        );
        return;
    };
    let allowed = state
        .sessions
        .get_mut(session_id)
        .map(|session| session.chat.allow(std::time::Instant::now()))
        .unwrap_or(false);
    if !allowed {
        send_error_locked(&state, session_id, ErrorCode::RateLimit, "slow down");
        return;
    }
    let name = match state.sessions.get(session_id) {
        Some(session) => session.name.clone(),
        None => return,
    };
    let message_id = state.next_message_id();
    let message = ChatMessage {
        message_id,
        room_id: room.clone(),
        from: session_id.to_owned(),
        name,
        text: text.to_owned(),
        ts: now_ms(),
        client_msg_id: request.client_msg_id,
    };
    state.broadcast_room(&room, payload_envelope(kind::CHAT_MESSAGE, &message), None);
}

fn handle_presence(shared: &Arc<Shared>, session_id: &str, request: PresenceUpdate) {
    let mut state = shared.lock_state();
    let Some(room) = current_room(&state, session_id) else {
        send_error_locked(
            &state,
            session_id,
            ErrorCode::NotInRoom,
            "join a room first",
        );
        return;
    };
    let Some(session) = state.sessions.get_mut(session_id) else {
        return;
    };
    if let Some(value) = request.self_mute {
        session.self_mute = value;
    }
    if let Some(value) = request.self_deaf {
        session.self_deaf = value;
    }
    if let Some(value) = request.sharing {
        session.sharing = value;
    }
    if let Some(value) = request.voice {
        session.voice = value;
    }
    let update = MemberUpdated {
        session_id: session.id.clone(),
        self_mute: session.self_mute,
        self_deaf: session.self_deaf,
        sharing: session.sharing,
        voice: session.voice,
    };
    state.broadcast_room(&room, payload_envelope(kind::MEMBER_UPDATED, &update), None);
}

fn handle_signal(shared: &Arc<Shared>, session_id: &str, request: RtcSignal) {
    let size = serde_json::to_vec(&request.signal)
        .map(|bytes| bytes.len())
        .unwrap_or(usize::MAX);
    if size > rust_chat_protocol::MAX_SIGNAL_BYTES {
        send_error_to(
            shared,
            session_id,
            ErrorCode::PayloadTooLarge,
            "signaling payload is too large",
            false,
            false,
        );
        return;
    }
    let mut state = shared.lock_state();
    let Some(room) = current_room(&state, session_id) else {
        send_error_locked(
            &state,
            session_id,
            ErrorCode::NotInRoom,
            "join a room first",
        );
        return;
    };
    let Some(target_room) = state
        .sessions
        .get(&request.to)
        .map(|session| session.room.clone())
    else {
        send_error_locked(
            &state,
            session_id,
            ErrorCode::BadTarget,
            "target is offline",
        );
        return;
    };
    if target_room.as_deref() != Some(room.as_str()) {
        send_error_locked(
            &state,
            session_id,
            ErrorCode::BadTarget,
            "target is outside the room",
        );
        return;
    }
    let allowed = state
        .sessions
        .get_mut(session_id)
        .map(|session| session.signal.allow(std::time::Instant::now()))
        .unwrap_or(false);
    if !allowed {
        send_error_locked(&state, session_id, ErrorCode::RateLimit, "slow down");
        return;
    }
    let forward = RtcSignalForward {
        from: session_id.to_owned(),
        signal: request.signal,
    };
    let _ = state.send(&request.to, payload_envelope(kind::RTC_SIGNAL, &forward));
}

fn handle_file_offer(shared: &Arc<Shared>, session_id: &str, request: FileOffer) {
    if request.name.chars().count() > rust_chat_protocol::MAX_FILE_NAME_CHARS {
        send_error_to(
            shared,
            session_id,
            ErrorCode::BadPayload,
            "file name is too long",
            false,
            false,
        );
        return;
    }
    if request.size > rust_chat_protocol::MAX_FILE_BYTES {
        send_error_to(
            shared,
            session_id,
            ErrorCode::BadPayload,
            "file is too large",
            false,
            false,
        );
        return;
    }
    if request.sha256.len() != 64 || !request.sha256.chars().all(|ch| ch.is_ascii_hexdigit()) {
        send_error_to(
            shared,
            session_id,
            ErrorCode::BadPayload,
            "sha256 must be 64 hex characters",
            false,
            false,
        );
        return;
    }
    let mut state = shared.lock_state();
    let Some(room) = current_room(&state, session_id) else {
        send_error_locked(
            &state,
            session_id,
            ErrorCode::NotInRoom,
            "join a room first",
        );
        return;
    };
    let allowed = state
        .sessions
        .get_mut(session_id)
        .map(|session| session.offers.allow(std::time::Instant::now()))
        .unwrap_or(false);
    if !allowed {
        send_error_locked(
            &state,
            session_id,
            ErrorCode::RateLimit,
            "too many file offers",
        );
        return;
    }
    let payload = serde_json::json!({
        "from": session_id,
        "transferId": request.transfer_id,
        "name": request.name,
        "size": request.size,
        "sha256": request.sha256,
    });
    state.broadcast_room(&room, payload_envelope(kind::FILE_OFFER, &payload), None);
}

fn relay_file(
    shared: &Arc<Shared>,
    session_id: &str,
    target: &str,
    kind_name: &str,
    payload: serde_json::Value,
) {
    let state = shared.lock_state();
    let Some(room) = current_room(&state, session_id) else {
        send_error_locked(
            &state,
            session_id,
            ErrorCode::NotInRoom,
            "join a room first",
        );
        return;
    };
    let target_room = state
        .sessions
        .get(target)
        .map(|session| session.room.clone());
    match target_room {
        Some(Some(target_room)) if target_room == room => {
            let envelope = Envelope::new(kind_name, payload);
            let _ = state.send(target, envelope);
        }
        _ => send_error_locked(
            &state,
            session_id,
            ErrorCode::BadTarget,
            "target is outside the room",
        ),
    }
}

fn current_room(state: &State, session_id: &str) -> Option<String> {
    state.sessions.get(session_id)?.room.clone()
}

fn unique_name(state: &State, base: &str) -> String {
    if !state.sessions.values().any(|session| session.name == base) {
        return base.to_owned();
    }
    for index in 2..1000 {
        let suffix = format!(" ({index})");
        let keep = rust_chat_protocol::MAX_NAME_CHARS.saturating_sub(suffix.chars().count());
        let candidate: String = base.chars().take(keep).collect::<String>() + &suffix;
        if !state
            .sessions
            .values()
            .any(|session| session.name == candidate)
        {
            return candidate;
        }
    }
    format!("{base} (dup)")
}

fn parse<T: serde::de::DeserializeOwned>(envelope: &Envelope) -> Result<T, AuthFailure> {
    serde_json::from_value(envelope.payload.clone()).map_err(|error| AuthFailure {
        code: ErrorCode::BadPayload,
        message: format!("invalid payload: {error}"),
    })
}

async fn next_payload<T: serde::de::DeserializeOwned>(
    stream: &mut futures_util::stream::SplitStream<WebSocket>,
    expected: &str,
) -> Result<T, AuthFailure> {
    let deadline = tokio::time::Instant::now() + HANDSHAKE_TIMEOUT;
    loop {
        let message = tokio::time::timeout_at(deadline, stream.next())
            .await
            .map_err(|_| AuthFailure {
                code: ErrorCode::AuthRequired,
                message: "handshake timed out".into(),
            })?;
        match message {
            Some(Ok(Message::Text(text))) => {
                let envelope = Envelope::decode(text.as_bytes()).map_err(|error| AuthFailure {
                    code: ErrorCode::BadPayload,
                    message: error.to_string(),
                })?;
                if envelope.kind != expected {
                    return Err(AuthFailure {
                        code: ErrorCode::BadPayload,
                        message: format!("expected `{expected}`, got `{}`", envelope.kind),
                    });
                }
                return parse::<T>(&envelope);
            }
            Some(Ok(Message::Close(_))) | None => {
                return Err(AuthFailure {
                    code: ErrorCode::AuthRequired,
                    message: "connection closed during the handshake".into(),
                });
            }
            Some(Ok(_)) => continue,
            Some(Err(error)) => {
                return Err(AuthFailure {
                    code: ErrorCode::AuthRequired,
                    message: error.to_string(),
                });
            }
        }
    }
}

fn direct(tx: &mpsc::Sender<Envelope>, envelope: Envelope) {
    let _ = tx.try_send(envelope);
}

fn send_failure(tx: &mpsc::Sender<Envelope>, failure: AuthFailure) {
    let payload = ErrorPayload {
        code: failure.code,
        message: failure.message,
        retryable: false,
        fatal: true,
    };
    direct(tx, payload_envelope(kind::ERROR, &payload));
}

fn send_failure_to(shared: &Arc<Shared>, session_id: &str, failure: AuthFailure) {
    send_error_to(
        shared,
        session_id,
        failure.code,
        &failure.message,
        false,
        false,
    );
}

fn send_error_to(
    shared: &Arc<Shared>,
    session_id: &str,
    code: ErrorCode,
    message: &str,
    retryable: bool,
    fatal: bool,
) {
    let payload = ErrorPayload {
        code,
        message: message.to_owned(),
        retryable,
        fatal,
    };
    let state = shared.lock_state();
    send_payload_locked(&state, session_id, kind::ERROR, &payload);
}

fn send_error_locked(state: &State, session_id: &str, code: ErrorCode, message: &str) {
    let payload = ErrorPayload {
        code,
        message: message.to_owned(),
        retryable: false,
        fatal: false,
    };
    send_payload_locked(state, session_id, kind::ERROR, &payload);
}

fn send_payload<T: serde::Serialize>(
    shared: &Arc<Shared>,
    session_id: &str,
    kind_name: &str,
    payload: &T,
) {
    let state = shared.lock_state();
    send_payload_locked(&state, session_id, kind_name, payload);
}

fn send_payload_locked<T: serde::Serialize>(
    state: &State,
    session_id: &str,
    kind_name: &str,
    payload: &T,
) {
    let _ = state.send(session_id, payload_envelope(kind_name, payload));
}

fn send_error(
    tx: &mpsc::Sender<Envelope>,
    code: ErrorCode,
    message: impl Into<String>,
    retryable: bool,
    fatal: bool,
) {
    let payload = ErrorPayload {
        code,
        message: message.into(),
        retryable,
        fatal,
    };
    direct(tx, payload_envelope(kind::ERROR, &payload));
}

pub async fn run(shared: Arc<Shared>) -> anyhow::Result<()> {
    let address: SocketAddr = match format!("{}:{}", shared.config.host, shared.config.port).parse()
    {
        Ok(address) => address,
        Err(_) => tokio::net::lookup_host((shared.config.host.as_str(), shared.config.port))
            .await
            .map_err(|error| anyhow::anyhow!("cannot resolve {}: {error}", shared.config.host))?
            .next()
            .ok_or_else(|| anyhow::anyhow!("{} resolved to no address", shared.config.host))?,
    };
    let app = router(shared.clone());

    tracing::info!(
        "rust-chat-server '{}' listening on {} (tls={})",
        shared.config.server_name,
        address,
        shared.config.tls.enabled
    );
    if let Some(turn) = &shared.config.turn {
        tracing::info!("turn relay: {}", turn.urls.join(", "));
    } else {
        tracing::warn!("turn relay not configured: peers behind NAT cannot connect");
    }

    let serve = async {
        if shared.config.tls.enabled {
            let tls = axum_server::tls_rustls::RustlsConfig::from_pem_file(
                &shared.config.tls.cert_file,
                &shared.config.tls.key_file,
            )
            .await?;
            axum_server::bind_rustls(address, tls)
                .serve(app.into_make_service_with_connect_info::<SocketAddr>())
                .await?;
        } else {
            axum_server::bind(address)
                .serve(app.into_make_service_with_connect_info::<SocketAddr>())
                .await?;
        }
        Ok::<(), anyhow::Error>(())
    };

    tokio::select! {
        result = serve => result?,
        _ = tokio::signal::ctrl_c() => {
            tracing::info!("shutting down");
            let envelope = payload_envelope(
                kind::SERVER_SHUTDOWN,
                &ServerShutdown { in_ms: 1_000, reason: "server shutdown".into() },
            );
            shared.lock_state().broadcast_all(envelope);
            tokio::time::sleep(Duration::from_millis(1_000)).await;
        }
    }
    Ok(())
}

/// In-process plain-TCP server used by the integration tests.
pub async fn serve_test(
    shared: Arc<Shared>,
    listener: tokio::net::TcpListener,
) -> anyhow::Result<()> {
    axum::serve(
        listener,
        router(shared).into_make_service_with_connect_info::<SocketAddr>(),
    )
    .await?;
    Ok(())
}
