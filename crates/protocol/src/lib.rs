//! Wire types for protocol v2.
//!
//! The Kotlin project is the feature reference, not the compatibility target: v2 keeps the
//! proven semantics (rooms, SCRAM, opaque SDP relay) and drops the legacy capability dance.
//! See `docs/PROTOCOL.md`.

use serde::{Deserialize, Serialize};
use std::fmt;

/// Only this version is accepted. Bumped only for breaking changes.
pub const PROTOCOL_VERSION: u16 = 2;

// ---------------------------------------------------------------------------- limits

pub const MAX_FRAME_BYTES: usize = 64 * 1024;
pub const MAX_CHAT_BYTES: usize = 4096;
pub const MAX_SIGNAL_BYTES: usize = 32 * 1024;
pub const MAX_NAME_CHARS: usize = 32;
pub const MAX_TOPIC_CHARS: usize = 128;
pub const MAX_FILE_NAME_CHARS: usize = 255;
pub const MAX_FILE_BYTES: u64 = 8 * 1024 * 1024 * 1024;
pub const DEFAULT_MAX_MEMBERS_PER_ROOM: usize = 8;
pub const DEFAULT_MAX_ROOMS: usize = 32;

// ---------------------------------------------------------------------------- envelope

fn empty_object() -> serde_json::Value {
    serde_json::Value::Object(serde_json::Map::new())
}

/// Every frame is one JSON object: `{v, type, id?, replyTo?, payload}`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Envelope {
    pub v: u16,
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reply_to: Option<String>,
    #[serde(default = "empty_object")]
    pub payload: serde_json::Value,
}

impl Envelope {
    pub fn new(kind: impl Into<String>, payload: serde_json::Value) -> Self {
        Self {
            v: PROTOCOL_VERSION,
            kind: kind.into(),
            id: None,
            reply_to: None,
            payload,
        }
    }

    pub fn decode(text: &[u8]) -> Result<Self, ProtocolError> {
        if text.len() > MAX_FRAME_BYTES {
            return Err(ProtocolError::FrameTooLarge);
        }
        let value: Self = serde_json::from_slice(text)?;
        if value.v != PROTOCOL_VERSION {
            return Err(ProtocolError::Version(value.v));
        }
        Ok(value)
    }

    pub fn encode(&self) -> Result<Vec<u8>, ProtocolError> {
        let bytes = serde_json::to_vec(self)?;
        if bytes.len() > MAX_FRAME_BYTES {
            return Err(ProtocolError::FrameTooLarge);
        }
        Ok(bytes)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ProtocolError {
    #[error("frame exceeds {MAX_FRAME_BYTES} bytes")]
    FrameTooLarge,
    #[error("unsupported protocol version {0}")]
    Version(u16),
    #[error("invalid frame: {0}")]
    Json(#[from] serde_json::Error),
}

// ---------------------------------------------------------------------------- message types

pub mod kind {
    // client -> server
    pub const HELLO: &str = "hello";
    pub const AUTH: &str = "auth";
    pub const ROOM_LIST: &str = "room.list";
    pub const ROOM_JOIN: &str = "room.join";
    pub const ROOM_LEAVE: &str = "room.leave";
    pub const ROOM_CREATE: &str = "room.create";
    pub const ROOM_DELETE: &str = "room.delete";
    pub const CHAT_SEND: &str = "chat.send";
    pub const PRESENCE_UPDATE: &str = "presence.update";
    pub const RTC_SIGNAL: &str = "rtc.signal";
    pub const FILE_OFFER: &str = "file.offer";
    pub const FILE_ACCEPT: &str = "file.accept";
    pub const FILE_DECLINE: &str = "file.decline";
    pub const FILE_CANCEL: &str = "file.cancel";
    pub const PING: &str = "ping";

    // server -> client
    pub const CHALLENGE: &str = "challenge";
    pub const WELCOME: &str = "welcome";
    pub const ROOM_LIST_RESULT: &str = "room.list.result";
    pub const ROOM_STATE: &str = "room.state";
    pub const ROOM_LEFT: &str = "room.left";
    pub const ROOM_DELETED: &str = "room.deleted";
    pub const ROOM_FLAGS: &str = "room.flags";
    pub const MEMBER_JOINED: &str = "member.joined";
    pub const MEMBER_LEFT: &str = "member.left";
    pub const MEMBER_UPDATED: &str = "member.updated";
    pub const CHAT_MESSAGE: &str = "chat.message";
    pub const ERROR: &str = "error";
    pub const PONG: &str = "pong";
    pub const SERVER_SHUTDOWN: &str = "server.shutdown";
}

// ---------------------------------------------------------------------------- DTOs

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct KdfParams {
    pub alg: String,
    pub salt: String,
    pub m: u32,
    pub t: u32,
    pub p: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Member {
    pub session_id: String,
    pub name: String,
    pub pubkey_fp: String,
    pub joined_at: i64,
    #[serde(default)]
    pub self_mute: bool,
    #[serde(default)]
    pub self_deaf: bool,
    #[serde(default)]
    pub sharing: bool,
    #[serde(default)]
    pub capabilities: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RoomSummary {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub topic: String,
    pub member_count: usize,
    pub cap: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RoomInfo {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub topic: String,
    #[serde(default)]
    pub voice: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RoomState {
    pub room: RoomInfo,
    #[serde(rename = "self")]
    pub self_member: Member,
    pub members: Vec<Member>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatMessage {
    pub message_id: String,
    pub room_id: String,
    pub from: String,
    pub name: String,
    pub text: String,
    pub ts: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_msg_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ErrorPayload {
    pub code: ErrorCode,
    pub message: String,
    #[serde(default)]
    pub retryable: bool,
    #[serde(default)]
    pub fatal: bool,
}

/// Server error codes. Kept as an enum so both ends agree; unknown codes decode to `Internal`
/// only through [`ErrorCode::from_wire`], never silently in serde.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ErrorCode {
    #[serde(rename = "E_PROTO_VERSION")]
    ProtoVersion,
    #[serde(rename = "E_BAD_PAYLOAD")]
    BadPayload,
    #[serde(rename = "E_AUTH_REQUIRED")]
    AuthRequired,
    #[serde(rename = "E_AUTH_FAILED")]
    AuthFailed,
    #[serde(rename = "E_AUTH_RATE_LIMIT")]
    AuthRateLimit,
    #[serde(rename = "E_NAME_INVALID")]
    NameInvalid,
    #[serde(rename = "E_ROOM_NOT_FOUND")]
    RoomNotFound,
    #[serde(rename = "E_ROOM_DELETE_DENIED")]
    RoomDeleteDenied,
    #[serde(rename = "E_ROOM_NOT_EMPTY")]
    RoomNotEmpty,
    #[serde(rename = "E_ROOM_FULL")]
    RoomFull,
    #[serde(rename = "E_NOT_IN_ROOM")]
    NotInRoom,
    #[serde(rename = "E_BAD_TARGET")]
    BadTarget,
    #[serde(rename = "E_RATE_LIMIT")]
    RateLimit,
    #[serde(rename = "E_PAYLOAD_TOO_LARGE")]
    PayloadTooLarge,
    #[serde(rename = "E_INTERNAL")]
    Internal,
    #[serde(rename = "E_SHUTTING_DOWN")]
    ShuttingDown,
}

impl fmt::Display for ErrorCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let text = serde_json::to_value(self)
            .ok()
            .and_then(|v| v.as_str().map(str::to_owned))
            .unwrap_or_else(|| "E_INTERNAL".to_owned());
        f.write_str(&text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn envelope_round_trip() {
        let mut envelope = Envelope::new(kind::CHAT_SEND, serde_json::json!({"text": "hi"}));
        envelope.id = Some("c-1".into());
        let bytes = envelope.encode().unwrap();
        let decoded = Envelope::decode(&bytes).unwrap();
        assert_eq!(decoded, envelope);
    }

    #[test]
    fn envelope_rejects_other_versions() {
        let bytes = br#"{"v":1,"type":"ping","payload":{}}"#;
        assert!(matches!(
            Envelope::decode(bytes),
            Err(ProtocolError::Version(1))
        ));
    }

    #[test]
    fn envelope_rejects_oversized_frames() {
        let big = vec![b' '; MAX_FRAME_BYTES + 1];
        assert!(matches!(
            Envelope::decode(&big),
            Err(ProtocolError::FrameTooLarge)
        ));
    }

    #[test]
    fn chat_message_uses_camel_case_on_the_wire() {
        let message = ChatMessage {
            message_id: "m1".into(),
            room_id: "lobby".into(),
            from: "s1".into(),
            name: "alice".into(),
            text: "hello".into(),
            ts: 1_763_000_000_000,
            client_msg_id: Some("c-1".into()),
        };
        let json = serde_json::to_value(&message).unwrap();
        assert_eq!(json["messageId"], "m1");
        assert_eq!(json["clientMsgId"], "c-1");
    }

    #[test]
    fn error_codes_have_the_documented_wire_names() {
        assert_eq!(ErrorCode::AuthFailed.to_string(), "E_AUTH_FAILED");
        assert_eq!(ErrorCode::RoomFull.to_string(), "E_ROOM_FULL");
    }

    #[test]
    fn room_state_renames_self() {
        let member = Member {
            session_id: "s1".into(),
            name: "alice".into(),
            pubkey_fp: "SHA256:00".into(),
            joined_at: 1,
            self_mute: false,
            self_deaf: false,
            sharing: false,
            capabilities: Vec::new(),
        };
        let state = RoomState {
            room: RoomInfo {
                id: "lobby".into(),
                name: "Lobby".into(),
                topic: String::new(),
                voice: true,
            },
            self_member: member.clone(),
            members: vec![member],
        };
        let json = serde_json::to_value(&state).unwrap();
        assert!(json.get("self").is_some());
        assert!(json.get("selfMember").is_none());
        assert_eq!(json["room"]["name"], "Lobby");
    }
}
