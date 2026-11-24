//! Client state shared by the desktop and Android apps.
//!
//! M0 defines the connection state machine and the reconnect schedule; the WebSocket client
//! and room store land in M2.

use rust_chat_protocol::{Member, RoomSummary};
use std::time::Duration;

/// A connection problem in the shape a UI can show.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnectionFailure {
    pub summary: String,
    pub hint: Option<String>,
    pub detail: Option<String>,
}

impl ConnectionFailure {
    pub fn new(summary: impl Into<String>) -> Self {
        Self {
            summary: summary.into(),
            hint: None,
            detail: None,
        }
    }

    pub fn with_hint(mut self, hint: impl Into<String>) -> Self {
        self.hint = Some(hint.into());
        self
    }
}

/// Coarse connection state surfaced to the UI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConnectionState {
    Idle,
    Connecting {
        endpoint: String,
    },
    Authenticating,
    Ready,
    Reconnecting {
        attempt: u32,
        delay: Duration,
        cause: Option<String>,
        hint: Option<String>,
    },
    Failed(ConnectionFailure),
}

impl ConnectionState {
    pub fn is_ready(&self) -> bool {
        matches!(self, Self::Ready)
    }

    pub fn label(&self) -> String {
        match self {
            Self::Idle => "not connected".into(),
            Self::Connecting { endpoint } => format!("connecting to {endpoint}"),
            Self::Authenticating => "authenticating".into(),
            Self::Ready => "connected".into(),
            Self::Reconnecting {
                attempt,
                delay,
                cause,
                ..
            } => {
                let mut text = format!("reconnecting (attempt {attempt} in {}s)", delay.as_secs());
                if let Some(cause) = cause.as_deref().filter(|cause| !cause.trim().is_empty()) {
                    text.push_str(": ");
                    text.push_str(cause);
                }
                text
            }
            Self::Failed(failure) => format!("failed: {}", failure.summary),
        }
    }
}

/// Exponential backoff, 0.5 s → 30 s. Jitter is applied by the caller (0.8..=1.2).
pub fn base_backoff(attempt: u32) -> Duration {
    let exponent = attempt.saturating_sub(1).min(6);
    let millis = 500u64.saturating_mul(1u64 << exponent);
    Duration::from_millis(millis.min(30_000))
}

pub fn backoff_with_jitter(attempt: u32, jitter: f32) -> Duration {
    let factor = jitter.clamp(0.8, 1.2);
    let millis = (base_backoff(attempt).as_millis() as f32 * factor).round() as u64;
    Duration::from_millis(millis)
}

/// The authoritative room view after `room.state`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RoomSnapshot {
    pub room_id: Option<String>,
    pub room_name: String,
    pub room_topic: String,
    pub members: Vec<Member>,
    pub rooms: Vec<RoomSummary>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_grows_and_caps_at_thirty_seconds() {
        assert_eq!(base_backoff(1), Duration::from_millis(500));
        assert_eq!(base_backoff(2), Duration::from_millis(1_000));
        assert_eq!(base_backoff(3), Duration::from_millis(2_000));
        assert_eq!(base_backoff(7), Duration::from_millis(30_000));
        assert_eq!(base_backoff(50), Duration::from_millis(30_000));
    }

    #[test]
    fn jitter_stays_within_twenty_percent() {
        for jitter in [0.8, 0.9, 1.0, 1.1, 1.2] {
            let delay = backoff_with_jitter(3, jitter);
            assert!(delay >= Duration::from_millis(1_600));
            assert!(delay <= Duration::from_millis(2_400));
        }
        assert_eq!(backoff_with_jitter(3, 5.0), Duration::from_millis(2_400));
    }

    #[test]
    fn labels_match_the_old_client() {
        assert_eq!(ConnectionState::Idle.label(), "not connected");
        assert_eq!(ConnectionState::Ready.label(), "connected");
        assert_eq!(
            ConnectionState::Connecting {
                endpoint: "127.0.0.1:8443".into()
            }
            .label(),
            "connecting to 127.0.0.1:8443"
        );
        assert_eq!(
            ConnectionState::Reconnecting {
                attempt: 2,
                delay: Duration::from_secs(1),
                cause: Some("timeout".into()),
                hint: None,
            }
            .label(),
            "reconnecting (attempt 2 in 1s): timeout"
        );
    }
}
