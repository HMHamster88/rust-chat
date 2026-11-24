//! Desktop media backend: cpal/WASAPI, windows-capture, SendInput, wgpu. Lands in M3.

pub use rust_chat_media::{AudioDevice, AudioDevices, CaptureSource, MediaEngine, PeerId};

/// Placeholder so the workspace compiles before the M3 media spike.
#[derive(Debug, Default)]
pub struct DesktopMediaEngine;

/// Screen control is only advertised by hosts with an input backend.
pub const SCREEN_CONTROL_SUPPORTED: bool = cfg!(windows);
