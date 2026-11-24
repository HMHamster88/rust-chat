//! Desktop entry point. The window arrives with the M0 toolkit spike and the M2 client shell.

fn main() {
    println!(
        "{} {} (M0 scaffold) — protocol v{}; media backend: {}",
        rust_chat_ui::APP_NAME,
        env!("CARGO_PKG_VERSION"),
        rust_chat_protocol::PROTOCOL_VERSION,
        if rust_chat_media_desktop::SCREEN_CONTROL_SUPPORTED {
            "desktop (screen control enabled)"
        } else {
            "desktop"
        }
    );
}
