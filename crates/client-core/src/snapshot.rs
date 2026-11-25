//! Everything the UI renders, updated by the client task.

use crate::ConnectionState;
use rust_chat_protocol::payloads::TurnCredentials;
use rust_chat_protocol::{ChatMessage, RoomState, RoomSummary};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrustPrompt {
    pub fingerprint: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatSnapshot {
    pub connection: ConnectionState,
    pub server_name: String,
    pub motd: String,
    pub server_fingerprint: Option<String>,
    pub self_id: Option<String>,
    pub rooms: Vec<RoomSummary>,
    pub room: Option<RoomState>,
    pub messages: Vec<ChatMessage>,
    pub deletable_rooms: Vec<String>,
    pub last_error: Option<String>,
    pub trust: Option<TrustPrompt>,
    pub latency_ms: Option<i64>,
    pub max_chat_bytes: usize,
    pub roomless: bool,
    pub turn: Option<TurnCredentials>,
}

impl Default for ChatSnapshot {
    fn default() -> Self {
        Self {
            connection: ConnectionState::Idle,
            server_name: String::new(),
            motd: String::new(),
            server_fingerprint: None,
            self_id: None,
            rooms: Vec::new(),
            room: None,
            messages: Vec::new(),
            deletable_rooms: Vec::new(),
            last_error: None,
            trust: None,
            latency_ms: None,
            max_chat_bytes: 4096,
            roomless: false,
            turn: None,
        }
    }
}

impl ChatSnapshot {
    pub fn room_id(&self) -> Option<&str> {
        self.room.as_ref().map(|state| state.room.id.as_str())
    }

    pub fn self_name(&self) -> Option<&str> {
        self.room
            .as_ref()
            .map(|state| state.self_member.name.as_str())
    }
}
