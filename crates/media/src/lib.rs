//! Platform-independent media seam.
//!
//! Everything platform-specific (cpal/WASAPI, windows-capture, SendInput, wgpu, AAudio) stays
//! behind this trait, mirroring the Kotlin `MediaEngine` boundary. The WebRTC implementation is
//! chosen in M3; the trait is deliberately small enough to survive that decision.

pub mod audio;
pub mod video;
pub mod voice;

use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PeerId(pub String);

impl fmt::Display for PeerId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioDevice {
    pub id: String,
    pub name: String,
    pub is_default: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AudioDevices {
    pub inputs: Vec<AudioDevice>,
    pub outputs: Vec<AudioDevice>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CaptureSource {
    pub id: String,
    pub title: String,
    pub is_window: bool,
}

/// I420 frame at the sender's resolution; the UI uploads it as a texture.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VideoFrame {
    pub width: u32,
    pub height: u32,
    pub timestamp_ms: u64,
    pub i420: Vec<u8>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PeerLinkState {
    New,
    Connecting,
    Connected,
    Relayed,
    Failed,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PeerStats {
    pub state: PeerLinkState,
    pub relayed: bool,
    pub rtt_ms: Option<u32>,
    pub jitter_ms: Option<u32>,
    pub packet_loss: Option<f32>,
}

/// One SDP/ICE payload, forwarded opaquely by the server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Signal {
    Offer {
        sdp: String,
    },
    Answer {
        sdp: String,
    },
    Ice {
        candidate: String,
        sdp_mid: Option<String>,
        sdp_m_line_index: Option<u16>,
    },
    Bye,
}

#[derive(Debug, thiserror::Error)]
pub enum MediaError {
    #[error("no capture source with id {0}")]
    UnknownSource(String),
    #[error("no audio device with id {0}")]
    UnknownDevice(String),
    #[error("media backend unavailable: {0}")]
    Unavailable(String),
    #[error("peer {0} is not attached")]
    UnknownPeer(PeerId),
    #[error("{0}")]
    Other(String),
}

pub type Result<T> = std::result::Result<T, MediaError>;

/// Events the engine pushes toward the client/UI. The backend feeds a channel in M3.
#[derive(Debug, Clone, PartialEq)]
pub enum MediaEvent {
    LocalLevel(f32),
    PeerLevel {
        peer: PeerId,
        level: f32,
    },
    PeerState {
        peer: PeerId,
        stats: PeerStats,
    },
    /// A new local offer/ICE candidate that must be sent to the peer through the server.
    Signal {
        peer: PeerId,
        signal: Signal,
    },
    RemoteFrame {
        peer: PeerId,
        frame: VideoFrame,
    },
    VoiceError(String),
    ShareError(String),
}

/// The seam every app wires to a concrete backend.
pub trait MediaEngine: Send + Sync {
    fn audio_devices(&self) -> AudioDevices;

    /// Device ids from [`Self::audio_devices`]; `None` keeps the current choice.
    fn set_audio_devices(&self, input: Option<&str>, output: Option<&str>) -> Result<()>;

    fn set_microphone_muted(&self, muted: bool);
    fn set_speaker_muted(&self, muted: bool);

    /// 0..=1 gains, applied live.
    fn set_gains(&self, microphone: f32, speaker: f32);

    fn attach_peer(&self, peer: &PeerId) -> Result<()>;
    fn detach_peer(&self, peer: &PeerId);
    fn handle_signal(&self, from: &PeerId, signal: Signal) -> Result<()>;

    fn capture_sources(&self) -> Result<Vec<CaptureSource>>;
    fn start_share(&self, source: &CaptureSource, max: (u32, u32), fps: u32) -> Result<()>;
    fn stop_share(&self);

    /// Latest decoded frame of the presenter, for the inline pane or the detached window.
    fn latest_remote_frame(&self, presenter: &PeerId) -> Option<VideoFrame>;

    fn stats(&self, peer: &PeerId) -> Option<PeerStats>;
}
