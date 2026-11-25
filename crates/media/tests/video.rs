//! Two-peer VP8 screen path: encode, RTP packetize, depacketize and decode.

use rust_chat_media::video::{VideoDecoder, VideoEncoder, bgra_to_i420, i420_to_bgra};
use rust_chat_media::voice::VoicePeer;
use std::time::Duration;

fn screen_pattern(width: u32, height: u32, frame: u32) -> Vec<u8> {
    let mut bgra = vec![0u8; width as usize * height as usize * 4];
    let box_x = (frame * 9) % (width - 80).max(1);
    for y in 0..height {
        for x in 0..width {
            let index = (y as usize * width as usize + x as usize) * 4;
            let paper = 235u8.saturating_sub(((x / 5 + y / 7) % 2) as u8 * 30);
            let mut color = [paper, paper.saturating_sub(2), paper.saturating_sub(5), 255];
            if x >= box_x && x < box_x + 80 && y > height / 3 && y < height / 3 + 60 {
                color = [40, 80, 220, 255];
            }
            bgra[index..index + 4].copy_from_slice(&color);
        }
    }
    bgra
}

#[tokio::test]
async fn two_peers_exchange_vp8_frames() {
    let mut a = VoicePeer::new("127.0.0.1:0").await.expect("peer A");
    let mut b = VoicePeer::new("127.0.0.1:0").await.expect("peer B");

    let offer = a.create_offer().await.expect("offer");
    let answer = b.accept_offer(&offer).await.expect("answer");
    a.accept_answer(&answer).await.expect("remote answer");
    a.wait_connected(Duration::from_secs(15))
        .await
        .expect("A connects");
    b.wait_connected(Duration::from_secs(15))
        .await
        .expect("B connects");
    a.prepare().await.expect("A media");
    b.prepare().await.expect("B media");

    let (width, height) = (640, 360);
    let mut encoder = VideoEncoder::new(width, height, 10, 800_000).expect("encoder");
    let mut receiver = b.take_received_video().expect("B video receiver");

    let mut sent = 0;
    let mut received = Vec::new();
    for frame in 0..20 {
        let bgra = screen_pattern(width, height, frame);
        let i420 = bgra_to_i420(&bgra, width, height);
        if let Some(encoded) = encoder.encode(&i420, frame == 0).expect("encode") {
            a.send_video(&encoded.data, Duration::from_millis(100))
                .await
                .expect("send video");
            sent += 1;
        }
        while let Ok(picture) = receiver.try_recv() {
            received.push(picture);
        }
        if received.len() >= 5 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    assert!(sent >= 5, "sent only {sent} frames");
    assert!(
        received.len() >= 3,
        "decoded only {} frames after {sent} sent",
        received.len()
    );
    let picture = received.last().unwrap();
    assert_eq!((picture.width, picture.height), (width, height));
    let pixels = i420_to_bgra(&picture.i420, picture.width, picture.height);
    assert_eq!(pixels.len(), width as usize * height as usize * 4);
    let distinct: std::collections::HashSet<u8> = pixels
        .as_chunks::<4>()
        .0
        .iter()
        .map(|pixel| pixel[2])
        .collect();
    assert!(distinct.len() > 4, "decoded frame was flat");

    // The same connection still carries audio.
    assert!(a.take_received().is_some());

    a.close().await;
    b.close().await;
}

#[test]
fn decoder_is_usable_after_reset() {
    let (width, height) = (320, 240);
    let mut encoder = VideoEncoder::new(width, height, 10, 300_000).unwrap();
    let bgra = screen_pattern(width, height, 0);
    let i420 = bgra_to_i420(&bgra, width, height);
    let frame = encoder.encode(&i420, false).unwrap().unwrap();
    let mut decoder = VideoDecoder::new();
    let first = decoder.decode(&frame.data).unwrap().unwrap();
    assert_eq!((first.width, first.height), (width, height));
    decoder.reset();
    let second = decoder.decode(&frame.data).unwrap().unwrap();
    assert_eq!((second.width, second.height), (width, height));
}
