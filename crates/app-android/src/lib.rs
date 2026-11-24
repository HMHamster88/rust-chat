//! Android entry point (cdylib consumed by the thin Kotlin/Java shell). Lands in M7.
//!
//! Keep this crate free of Android-only dependencies until the mobile milestone, so
//! `cargo check --workspace` stays green on desktop CI.

pub fn client_name() -> &'static str {
    rust_chat_ui::APP_NAME
}
