# WebRTC loopback M0 gate

Standalone Cargo project (`webrtc = "0.21"`) so the media stack can be proven before it is wired
into the product.

## Run

```powershell
cargo run
```

Expected output ends with:

```
DATA-CHANNEL OK: <n> bytes arrived intact
```

The spike creates two peer connections in one process, negotiates non-trickle over loopback,
opens a data channel from the offerer, and asserts a payload arrives intact.

## Result (2026-11-24)

**Passed.** On this machine (`webrtc 0.21.1`, Rust 1.100, Windows 11):

```
created data channel 'files'
[offerer] connection state: Connecting
[answerer] connection state: Connecting
[offerer] connection state: Connected
[answerer] connection state: Connected
[answerer] remote opened data channel 'files'
sent 46 bytes
DATA-CHANNEL OK: 46 bytes arrived intact
```

This proves the pure-Rust stack negotiates DTLS/SCTP and moves bytes; Opus, jitter buffering,
AEC and VP8 remain the M3 gate.

## Next (M3)

1. Opus encode/decode around the same two peers (48 kHz mono, 20 ms frames).
2. A jitter buffer and `webrtc-audio-processing` AEC with a synthetic far-end reference.
3. A 720p10 VP8 track: encode once, deliver to a second peer, request a keyframe, measure CPU.
4. Two-process loopback so the driver threads and RSS can be measured per client.
