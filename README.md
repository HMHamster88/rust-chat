# rust-chat

A self-hosted chat with text, voice, screen sharing, remote control and peer-to-peer file
transfer — rewritten from the Kotlin implementation in `../kotlin-chat`.

The server is a control plane: it authenticates sessions, owns rooms and presence, relays text,
and forwards opaque SDP/ICE. Media and files travel directly between clients over WebRTC
(DTLS-SRTP for voice/screen, SCTP data channels for files and screen control). The relay is
coturn, unchanged.

- Design and milestones: [docs/PLAN.md](docs/PLAN.md)
- Wire protocol v2: [docs/PROTOCOL.md](docs/PROTOCOL.md)
- Kotlin baseline (RAM/CPU): [docs/BASELINE.md](docs/BASELINE.md)

## Status

**M2 core complete — desktop shell.** The FerroUI/Direct2D client connects over WSS with TOFU
pinning, SCRAM auth, rooms, members and chat; local history survives a restart, the password can
live in the OS key store, and reconnect is automatic. Measured **98–99 MB** working set and under
1% idle CPU (the Kotlin client was 408–420 MB and ~9%). Next is M3 media. Settings-screen polish,
the update checker and audio devices remain for their milestones.

Protocol v2 is intentionally **not** compatible with the Kotlin clients. The Kotlin repository is
the feature specification, not a runtime peer.

## Layout

| Crate | Purpose |
|---|---|
| `crates/protocol` | Envelope, DTOs, errors, limits, version |
| `crates/crypto` | Argon2id verifier, SCRAM-SHA-256, fingerprints |
| `crates/server` | Control plane (axum/rustls in M1) and the `rust-chat-server` CLI |
| `crates/client-core` | State machine, reconnect, store, history, file transfer |
| `crates/media` | Platform-independent media seam |
| `crates/media-desktop` | cpal, windows-capture, SendInput, wgpu |
| `crates/media-android` | AAudio, platform AEC, GLES/Vulkan |
| `crates/ui` | Shared screens, widgets and theme |
| `crates/app-desktop` | `rust-chat` binary |
| `crates/app-android` | cdylib + thin Kotlin/Java shell |
| `spikes/` | M0 toolkit and media gates (separate Cargo projects, not product code) |

## Build

```powershell
cargo check --workspace
cargo test --workspace
```

Server first run:

```powershell
cargo run -p rust-chat-server -- init --password "choose-a-password"
cargo run -p rust-chat-server -- fingerprint
```

`init` writes `config/server.toml`, `config/verifier` (the same format as the Kotlin server, so an
existing verifier can be reused) and an ECDSA P-256 certificate under `config/tls/`. Clients pin
the printed SHA-256 fingerprint on first connect.

## License

Not chosen yet. `spikes/ferroui` is subject to FerroUI's MIT license; the product crates are
unlicensed until the owner picks one.
