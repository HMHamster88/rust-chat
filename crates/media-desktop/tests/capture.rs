//! Opt-in monitor capture check: proves WGC delivers a BGRA frame on this desktop.

use rust_chat_media_desktop::capture::ScreenCapture;
use std::time::Duration;

#[test]
#[ignore = "needs an interactive desktop"]
fn monitor_one_delivers_a_frame() {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let mut capture = ScreenCapture::start_monitor(1, tx).expect("capture starts");
    let frame = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .unwrap()
        .block_on(async {
            tokio::time::timeout(Duration::from_secs(8), rx.recv())
                .await
                .ok()
                .flatten()
        });
    if frame.is_none() {
        std::thread::sleep(Duration::from_secs(2));
    }
    capture.stop();
    let frame = frame.expect("a captured frame");
    println!(
        "captured {}x{} ({} bytes)",
        frame.width,
        frame.height,
        frame.bgra.len()
    );
    assert!(frame.width > 0 && frame.height > 0);
    assert_eq!(
        frame.bgra.len(),
        frame.width as usize * frame.height as usize * 4
    );
    assert!(frame.bgra.iter().any(|sample| *sample != 0));
}
