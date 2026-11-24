//! Android media backend: AAudio capture/playout, platform AEC, VP8 decode, GLES/Vulkan
//! presentation. Lands in M7; the crate exists now so the shared seam is the only dependency.

pub use rust_chat_media::{AudioDevice, AudioDevices, CaptureSource, MediaEngine, PeerId};
