//! Typed payloads for protocol v2. The envelope carries these as `payload` JSON objects.

use crate::{Envelope, KdfParams, Member, RoomState, RoomSummary};
use serde::{Deserialize, Serialize};

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Hello {
    pub protocol: u16,
    pub name: String,
    pub pubkey: String,
    #[serde(default)]
    pub features: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Challenge {
    pub nonce: String,
    pub kdf: KdfParams,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Auth {
    pub client_nonce: String,
    pub proof: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Limits {
    pub max_chat_bytes: usize,
    pub max_members_per_room: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TurnCredentials {
    pub urls: Vec<String>,
    pub username: String,
    pub credential: String,
    pub ttl_seconds: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Welcome {
    pub session_id: String,
    pub server_name: String,
    pub motd: String,
    pub protocol: u16,
    pub server_signature: String,
    pub rooms: Vec<RoomSummary>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn: Option<TurnCredentials>,
    pub limits: Limits,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RoomJoin {
    pub room_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RoomCreate {
    pub name: String,
    #[serde(default)]
    pub topic: String,
    #[serde(default = "default_true")]
    pub voice: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RoomDelete {
    pub room_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatSend {
    pub text: String,
    #[serde(default)]
    pub client_msg_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PresenceUpdate {
    #[serde(default)]
    pub self_mute: Option<bool>,
    #[serde(default)]
    pub self_deaf: Option<bool>,
    #[serde(default)]
    pub sharing: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RtcSignal {
    pub to: String,
    /// Opaque offer/answer/ICE body; the server never parses it.
    pub signal: serde_json::Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RtcSignalForward {
    pub from: String,
    pub signal: serde_json::Value,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileOffer {
    pub transfer_id: String,
    pub name: String,
    pub size: u64,
    pub sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileAccept {
    pub transfer_id: String,
    pub to: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileDecline {
    pub transfer_id: String,
    pub to: String,
    #[serde(default)]
    pub reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileCancel {
    pub transfer_id: String,
    #[serde(default)]
    pub to: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MemberUpdated {
    pub session_id: String,
    pub self_mute: bool,
    pub self_deaf: bool,
    pub sharing: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MemberLeft {
    pub session_id: String,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RoomFlags {
    pub deletable: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Pong {
    pub ts: i64,
    pub server_ts: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerShutdown {
    pub in_ms: u64,
    pub reason: String,
}

// A small set of constructors keep the server's `json!` usage consistent.
impl RoomState {
    pub fn members_with_self(&self) -> Vec<Member> {
        self.members.clone()
    }
}

impl Envelope {
    pub fn reply(kind: impl Into<String>, payload: serde_json::Value) -> Self {
        Self::new(kind, payload)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::RoomInfo;

    #[test]
    fn welcome_round_trips_with_and_without_turn() {
        let welcome = Welcome {
            session_id: "s_1".into(),
            server_name: "home".into(),
            motd: "hi".into(),
            protocol: crate::PROTOCOL_VERSION,
            server_signature: "AA==".into(),
            rooms: vec![RoomSummary {
                id: "lobby".into(),
                name: "Lobby".into(),
                topic: String::new(),
                member_count: 1,
                cap: 8,
            }],
            turn: None,
            limits: Limits {
                max_chat_bytes: 4096,
                max_members_per_room: 8,
            },
        };
        let json = serde_json::to_value(&welcome).unwrap();
        assert!(json.get("turn").is_none());
        assert_eq!(json["sessionId"], "s_1");
        assert_eq!(json["limits"]["maxChatBytes"], 4096);
        let decoded: Welcome = serde_json::from_value(json).unwrap();
        assert_eq!(decoded, welcome);
    }

    #[test]
    fn payloads_use_camel_case_on_the_wire() {
        let hello = Hello {
            protocol: 2,
            name: "alice".into(),
            pubkey: "pk".into(),
            features: vec!["video".into()],
        };
        let json = serde_json::to_value(&hello).unwrap();
        assert_eq!(json["protocol"], 2);
        assert_eq!(json["name"], "alice");
        assert_eq!(json["features"][0], "video");

        let send: ChatSend = serde_json::from_str(r#"{"text":"hi","clientMsgId":"c-1"}"#).unwrap();
        assert_eq!(send.client_msg_id.as_deref(), Some("c-1"));
    }

    #[test]
    fn presence_fields_are_optional_individually() {
        let update: PresenceUpdate = serde_json::from_str(r#"{"sharing":true}"#).unwrap();
        assert_eq!(update.sharing, Some(true));
        assert_eq!(update.self_mute, None);
    }

    #[test]
    fn room_create_defaults_to_voice() {
        let create: RoomCreate = serde_json::from_str(r#"{"name":"Dev"}"#).unwrap();
        assert!(create.voice);
        assert!(create.topic.is_empty());
    }

    #[test]
    fn opaque_signal_survives_round_trip() {
        let signal = RtcSignal {
            to: "s_2".into(),
            signal: serde_json::json!({
                "kind": "ice",
                "candidate": "candidate:1 1 UDP 1 127.0.0.1 5000 typ host",
                "sdpMid": "0",
                "sdpMLineIndex": 0
            }),
        };
        let json = serde_json::to_string(&signal).unwrap();
        let decoded: RtcSignal = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded, signal);
    }

    #[test]
    fn room_info_stays_the_shared_shape() {
        let info = RoomInfo {
            id: "lobby".into(),
            name: "Lobby".into(),
            topic: String::new(),
            voice: true,
        };
        assert_eq!(serde_json::to_value(info).unwrap()["voice"], true);
    }
}
