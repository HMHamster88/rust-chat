# Changelog

All notable changes to rust-chat. The format follows Keep a Changelog; versions are
`major.minor.patch` and tags are `vX.Y.Z`.

## [0.1.3] — 2026-11-25

### Added

- Server self-update (`rust-chat-server update`, `--check`, `--rollback`) and an Ubuntu installer
  (`scripts/install-server.sh`) with systemd, optional daily updates and coturn setup.
- `init --dir <dir> --data-dir <dir>` for the `/etc/rust-chat` + `/var/lib/rust-chat` layout, and
  `--password-file` for unattended provisioning.

### Notes

- The installer targets Ubuntu/Debian on x86_64; there is no APT repository and no ARM64 build.
- Automatic server updates are offered by the installer and default to enabled; coturn is offered
  and defaults to enabled.
- The Windows release remains the client only; the Linux release carries the server.

## [0.1.2] — 2026-11-25

### Changed

- Windows releases reuse the release client built on `main` (uploaded as a workflow artifact) and
  only fall back to building on the tag, so a release is seconds rather than ~16 minutes when the
  main build is green. The Linux server still builds during release.
- The Windows release contains only the client, README and SHA-256 sums.

## [0.1.1] — 2026-11-25

### Changed

- Windows releases now ship only the desktop client; `rust-chat-server.exe` is a development tool
  and is no longer packaged. The Linux server tarball is unchanged.
- Faster release builds: the `release` profile disables LTO and uses 16 codegen units, and CI and
  release jobs use `sccache` with the GitHub Actions cache. `--profile dist` still produces the
  thin-LTO binary for local use.
- Windows release packaging includes only the client, README and SHA-256 sums.

## [0.1.0] — 2026-11-25

First public build of the Rust rewrite. Feature parity with the Kotlin desktop client except
Android and a few polish items listed below.

### Added

- **Server:** self-hosted control plane with TLS, SCRAM-SHA-256 authentication (Argon2id
  verifier), rooms, presence, chat, opaque WebRTC signaling, file metadata relay, TURN REST
  credentials, rate limits, and a CLI (`init`, `hash-password`, `fingerprint`, `serve`).
- **Desktop client:** FerroUI/Direct2D shell with TOFU certificate pinning, saved server,
  optional password in the OS key store, automatic reconnect, rooms, chat, local history and a
  monitor/window capture picker.
- **Voice:** Opus mesh over WebRTC with pure-Rust APM (AEC3, noise suppression, AGC), jitter
  buffering, mute/deafen, speaking levels, per-peer RTT/relay labels, keyframe requests,
  peer rebuild with backoff, TURN support and automatic rejoin after a control-plane reconnect.
- **Screen sharing:** Windows Graphics Capture → VP8 (pure-Rust codec) at 720p/1080p/1440p and
  5–30 fps → Direct2D texture presentation without CPU readback.
- **Remote control:** consent, 5–60 minute lease, heartbeat, `SendInput` keyboard/mouse/wheel,
  Alt+F4 filter and release-all on end.
- **Files:** peer-to-peer transfer over a reliable data channel with captions, offer/accept,
  progress, cancel, SHA-256 verification, `.part` rename, numeric suffixes and Downloads.
- **Updates:** in-app GitHub release check and “Open release” action.
- **Packaging:** GitHub Actions CI and tagged Windows/Linux release artifacts with SHA-256 sums.

### Known gaps

- Android is planned last (M9) and is not in this release.
- No image previews, no download resume, no settings screen yet; 1080p/1440p is capped at
  15/10 fps by the pure-Rust VP8 encoder.
- Release binaries are unsigned; Windows SmartScreen may warn.
