//! Two-peer voice over a real WebRTC connection, both directions, with Opus.

use rust_chat_media::audio::{FRAME_SAMPLES, rms, sine};
use rust_chat_media::voice::VoicePeer;
use std::time::Duration;

#[tokio::test]
async fn two_peers_exchange_opus_audio_both_ways() {
    let mut a = VoicePeer::new("127.0.0.1:0").await.expect("peer A");
    let mut b = VoicePeer::new("127.0.0.1:0").await.expect("peer B");

    let offer = a.create_offer().await.expect("offer");
    assert!(offer.contains("m=audio"), "offer has no audio m-line");
    let answer = b.accept_offer(&offer).await.expect("answer");
    assert!(answer.contains("m=audio"), "answer has no audio m-line");
    a.accept_answer(&answer).await.expect("remote answer");

    a.wait_connected(Duration::from_secs(15))
        .await
        .expect("A connects");
    b.wait_connected(Duration::from_secs(15))
        .await
        .expect("B connects");
    a.prepare().await.expect("A media");
    b.prepare().await.expect("B media");

    let mut phase_a = 0.0;
    let mut phase_b = 1.0;
    for _ in 0..60 {
        a.send_pcm(&sine(&mut phase_a, 0.5)).await.expect("A sends");
        b.send_pcm(&sine(&mut phase_b, 0.5)).await.expect("B sends");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    let mut a_pcm = Vec::new();
    let mut b_pcm = Vec::new();
    let mut a_received = a.take_received().expect("A receiver");
    let mut b_received = b.take_received().expect("B receiver");
    let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
    while tokio::time::Instant::now() < deadline {
        tokio::select! {
            frame = a_received.recv() => {
                if let Some(frame) = frame {
                    a_pcm.extend(frame);
                } else {
                    break;
                }
            }
            frame = b_received.recv() => {
                if let Some(frame) = frame {
                    b_pcm.extend(frame);
                } else {
                    break;
                }
            }
            _ = tokio::time::sleep(Duration::from_millis(100)) => {
                if a_pcm.len() >= FRAME_SAMPLES * 10 && b_pcm.len() >= FRAME_SAMPLES * 10 {
                    break;
                }
            }
        }
    }

    assert!(
        a_pcm.len() >= FRAME_SAMPLES * 5,
        "A received only {} samples",
        a_pcm.len()
    );
    assert!(
        b_pcm.len() >= FRAME_SAMPLES * 5,
        "B received only {} samples",
        b_pcm.len()
    );
    assert!(rms(&a_pcm) > 0.1, "A audio RMS {}", rms(&a_pcm));
    assert!(rms(&b_pcm) > 0.1, "B audio RMS {}", rms(&b_pcm));

    a.close().await;
    b.close().await;
}
