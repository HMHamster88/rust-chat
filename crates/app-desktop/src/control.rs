//! Remote-control messages carried over the peer-to-peer `control` data channel.

use serde::{Deserialize, Serialize};

/// The default and limits from the feature list: 30 minutes, bounded to 5–60.
pub const DEFAULT_LEASE_SECS: u64 = 30 * 60;
pub const MIN_LEASE_SECS: u64 = 5 * 60;
pub const MAX_LEASE_SECS: u64 = 60 * 60;
pub const HEARTBEAT_SECS: u64 = 5;
pub const HEARTBEAT_TIMEOUT_SECS: u64 = 15;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum ControlMessage {
    Request { lease_secs: u64 },
    Grant { lease_secs: u64 },
    Deny { reason: String },
    Revoke { reason: String },
    Input { event: InputEvent },
    Heartbeat,
    End { reason: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum InputEvent {
    MouseMove {
        x: f32,
        y: f32,
    },
    MouseButton {
        button: RemoteButton,
        pressed: bool,
    },
    Wheel {
        delta: i32,
        #[serde(default)]
        horizontal: bool,
    },
    Key {
        vk: u16,
        pressed: bool,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RemoteButton {
    Left,
    Right,
    Middle,
}

impl ControlMessage {
    pub fn encode(&self) -> String {
        serde_json::to_string(self).unwrap_or_default()
    }

    pub fn decode(text: &str) -> Option<Self> {
        serde_json::from_str(text).ok()
    }
}

pub fn lease_secs(requested: u64) -> u64 {
    requested.clamp(MIN_LEASE_SECS, MAX_LEASE_SECS)
}

/// `m:ss` remaining, rounded up so a fresh lease never shows `0:00`.
pub fn countdown(seconds: u64) -> String {
    format!("{}:{:02}", seconds / 60, seconds % 60)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn messages_round_trip_on_the_wire() {
        let request = ControlMessage::Request {
            lease_secs: DEFAULT_LEASE_SECS,
        };
        let text = request.encode();
        assert!(text.contains("\"t\":\"request\""));
        assert_eq!(ControlMessage::decode(&text), Some(request));

        let input = ControlMessage::Input {
            event: InputEvent::MouseMove { x: 0.25, y: 0.75 },
        };
        let text = input.encode();
        assert!(text.contains("\"kind\":\"mouse_move\""));
        assert_eq!(ControlMessage::decode(&text), Some(input));
    }

    #[test]
    fn leases_are_clamped_to_five_through_sixty_minutes() {
        assert_eq!(lease_secs(1), MIN_LEASE_SECS);
        assert_eq!(lease_secs(DEFAULT_LEASE_SECS), DEFAULT_LEASE_SECS);
        assert_eq!(lease_secs(u64::MAX), MAX_LEASE_SECS);
        assert_eq!(countdown(59), "0:59");
        assert_eq!(countdown(1800), "30:00");
    }
}
