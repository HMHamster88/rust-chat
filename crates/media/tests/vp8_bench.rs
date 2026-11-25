//! Opt-in VP8 screen benchmark: `cargo test --release -p rust-chat-media --test vp8_bench -- --ignored --nocapture`.

use rust_chat_media::video::{VideoDecoder, VideoEncoder, bgra_to_i420, fit_bgra, i420_to_bgra};
use std::time::Instant;

fn screen_like(width: u32, height: u32, frame: u32) -> Vec<u8> {
    let mut bgra = vec![0u8; width as usize * height as usize * 4];
    let block_x = (frame * 23) % width.saturating_sub(160).max(1);
    let block_y = (frame * 11) % height.saturating_sub(120).max(1);
    for y in 0..height {
        for x in 0..width {
            let index = (y as usize * width as usize + x as usize) * 4;
            let text = ((x / 7 + y / 11) % 2 == 0) as u8 * 40;
            let mut color = [235u8, 238, 242, 255];
            if x >= block_x && x < block_x + 160 && y >= block_y && y < block_y + 120 {
                color = [40, 90, 220, 255];
            }
            bgra[index] = color[0].saturating_sub(text);
            bgra[index + 1] = color[1].saturating_sub(text);
            bgra[index + 2] = color[2].saturating_sub(text);
            bgra[index + 3] = color[3];
        }
    }
    bgra
}

#[test]
#[ignore = "slow; run explicitly in release mode"]
fn vp8_720p10_encoder_and_decoder_budget() {
    let (source_width, source_height) = (1920u32, 1080u32);
    let fps = 10u32;
    let frames = 60u32;
    let mut encoder = VideoEncoder::new(1280, 720, fps, 1_500_000).unwrap();
    let mut decoder = VideoDecoder::new();
    let mut encode_ms = Vec::new();
    let mut decode_ms = Vec::new();
    let mut bytes = 0usize;
    let mut keyframes = 0usize;
    for frame in 0..frames {
        let capture = screen_like(source_width, source_height, frame);
        let convert_start = Instant::now();
        let (scaled, width, height) = fit_bgra(&capture, source_width, source_height, 1280, 720);
        let i420 = bgra_to_i420(&scaled, width, height);
        let convert_ms = convert_start.elapsed().as_secs_f64() * 1000.0;

        let start = Instant::now();
        let encoded = encoder.encode(&i420, frame == 0).unwrap();
        encode_ms.push(start.elapsed().as_secs_f64() * 1000.0);
        let Some(encoded) = encoded else { continue };
        bytes += encoded.data.len();
        if encoded.keyframe {
            keyframes += 1;
        }
        assert!(encoded.data.len() < 400_000);

        let start = Instant::now();
        let decoded = decoder.decode(&encoded.data).unwrap().expect("a picture");
        decode_ms.push(start.elapsed().as_secs_f64() * 1000.0);
        let bgra = i420_to_bgra(&decoded.i420, decoded.width, decoded.height);
        assert_eq!(bgra.len(), 1280 * 720 * 4);
        assert_eq!((decoded.width, decoded.height), (1280, 720));
        if frame == 0 {
            println!(
                "first frame: layout convert {convert_ms:.2} ms, encode {:.2} ms, decode {:.2} ms, {} bytes",
                encode_ms[0],
                decode_ms[0],
                encoded.data.len()
            );
        }
    }
    let avg = |values: &[f64]| values.iter().sum::<f64>() / values.len() as f64;
    let max = |values: &[f64]| values.iter().cloned().fold(0.0, f64::max);
    println!(
        "frames={} keyframes={} bytes={} avg_bps={:.0}",
        frames,
        keyframes,
        bytes,
        (bytes as f64 * 8.0) / (frames as f64 / fps as f64)
    );
    println!(
        "encode: avg {:.2} ms, max {:.2} ms; decode: avg {:.2} ms, max {:.2} ms",
        avg(&encode_ms),
        max(&encode_ms),
        avg(&decode_ms),
        max(&decode_ms)
    );
    // 10 fps means a 100 ms budget per frame; leave headroom for capture and networking.
    assert!(
        avg(&encode_ms) < 45.0,
        "encoder is too slow: {:.2} ms average",
        avg(&encode_ms)
    );
    assert!(
        avg(&decode_ms) < 20.0,
        "decoder is too slow: {:.2} ms average",
        avg(&decode_ms)
    );
}
