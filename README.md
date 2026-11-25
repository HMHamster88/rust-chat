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
- Release history: [CHANGELOG.md](CHANGELOG.md)
- Release process: [docs/RELEASING.md](docs/RELEASING.md)

## Status

**M6 complete — file transfer.** Text, voice, screen sharing, remote control and files all work on
the desktop client and self-hosted server. Android is deliberately last (M9); next is desktop
polish (M7), then the release/update polish (M8). Tagged builds are produced by GitHub Actions and
the app checks GitHub for newer releases from the About row. See the changelog for the 0.1.0
feature set and known gaps.

Protocol v2 is intentionally **not** compatible with the Kotlin clients. The Kotlin repository is
the feature specification, not a runtime peer.

## Layout

| Crate | Purpose |
|---|---|
| `crates/protocol` | Envelope, DTOs, errors, limits, version |
| `crates/crypto` | Argon2id verifier, SCRAM-SHA-256, fingerprints |
| `crates/server` | Control plane (axum/rustls in M1) and the `rust-chat-server` CLI |
| `crates/client-core` | State machine, reconnect, store, history, file transfer |
| `crates/media` | Opus, VP8, jitter buffer, WebRTC peer, platform-independent media seam |
| `crates/media-desktop` | cpal/WASAPI audio, Sonora APM, Windows Graphics Capture |
| `crates/media-android` | AAudio, platform AEC, GLES/Vulkan |
| `crates/ui` | Shared screens, widgets and theme |
| `crates/app-desktop` | `rust-chat` binary |
| `crates/app-android` | cdylib + thin Kotlin/Java shell (M9, last) |
| `spikes/` | M0 toolkit and media gates (separate Cargo projects, not product code) |

## Build

```powershell
cargo check --workspace
cargo test --workspace
```

Release builds:

```powershell
cargo build --release -p rust-chat-desktop -p rust-chat-server
```

Tagged pushes (`vX.Y.Z`) build and attach the Windows client/server zip and the Linux server
tarball to a GitHub Release; the client's **Check for updates** button reads the latest release
from `HMHamster88/rust-chat`.

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
