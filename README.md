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
- Ubuntu VPS deployment plan: [docs/SERVER_DEPLOY.md](docs/SERVER_DEPLOY.md)

## Download

- **Windows (portable):** [rust-chat-v0.1.2-windows-x64.zip](https://github.com/HMHamster88/rust-chat/releases/download/v0.1.2/rust-chat-v0.1.2-windows-x64.zip)
  — `rust-chat.exe`, README and SHA-256 sums.
- **Linux server:** [rust-chat-v0.1.2-linux-x86_64.tar.gz](https://github.com/HMHamster88/rust-chat/releases/download/v0.1.2/rust-chat-v0.1.2-linux-x86_64.tar.gz)

The Windows server is a development tool and is not part of the Windows release. Windows artifacts
are built once on `main` and reused by tagged releases, so a release takes about two minutes when
the main build is green. Binaries are unsigned, so Windows SmartScreen may warn. The app checks
this same release feed from its update row and can open the release page.

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
cargo build --release -p rust-chat-desktop
```

The `release` profile is tuned for build speed (no LTO, 16 codegen units); `--profile dist` enables
thin LTO when you want the smallest binary. Tagged pushes (`vX.Y.Z`) build and attach the Windows
client zip and the Linux server tarball to a GitHub Release; the client's **Check for updates**
button reads the latest release from `HMHamster88/rust-chat`.

Server first run:

```powershell
cargo run -p rust-chat-server -- init --password "choose-a-password"
cargo run -p rust-chat-server -- fingerprint
```

`init` writes `config/server.toml`, `config/verifier` (the same format as the Kotlin server, so an
existing verifier can be reused) and an ECDSA P-256 certificate under `config/tls/`. Clients pin
the printed SHA-256 fingerprint on first connect.

## Server on a VPS

Ubuntu 22.04/24.04, x86_64:

```bash
curl -fsSL https://raw.githubusercontent.com/HMHamster88/rust-chat/main/scripts/install-server.sh | sudo bash
sudo rust-chat-server update            # later updates; --check and --rollback also exist
```

The installer sets up the systemd service, the `/etc/rust-chat` + `/var/lib/rust-chat` layout,
optional coturn and daily updates. Details, firewall ports and manual steps:
[docs/SERVER_DEPLOY.md](docs/SERVER_DEPLOY.md).

## License

Not chosen yet. `spikes/ferroui` is subject to FerroUI's MIT license; the product crates are
unlicensed until the owner picks one.
