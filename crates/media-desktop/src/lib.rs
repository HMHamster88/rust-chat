//! Desktop media backend: cpal/WASAPI audio, APM echo cancellation, screen capture.

pub mod audio;
pub mod capture;
pub mod input;

pub use rust_chat_media::{AudioDevice, AudioDevices, CaptureSource, MediaEngine, PeerId};

/// Screen control is only advertised by hosts with an input backend.
pub const SCREEN_CONTROL_SUPPORTED: bool = cfg!(windows);
