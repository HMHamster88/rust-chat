# rust-chat — Design Plan

A self-hosted chat with text, voice, screen sharing, remote control and peer-to-peer file
transfer, rewritten in Rust from `../kotlin-chat`.

The Kotlin project is the **feature specification**, not the compatibility target. No Kotlin
code, module layout, library choice or wire protocol is preserved. Protocol v2 is defined in
[`PROTOCOL.md`](./PROTOCOL.md).

- **Server:** Rust (tokio + axum + rustls), one static binary.
- **Clients:** Rust — desktop first (Windows, then Linux/macOS) and Android.
- **Media:** pure Rust WebRTC (`webrtc-rs`/`rtc`), Opus, VP8, WebRTC audio processing.
- **UI:** decided by the M0 FerroUI gate; fallback Slint or egui.
- **Status:** M0 in progress (scaffold, protocol/crypto foundations, server CLI, baseline,
  toolkit and media spikes).

---

## 1. Decisions

| Decision | Choice | Rationale |
|---|---|---|
| Scope | Client + server + Android in Rust | Owner decision; one language and one shared core |
| Wire compatibility | None; protocol v2 | No legacy hacks, no JVM client to keep alive |
| Media topology | WebRTC full mesh, server is a control plane | No SFU/media code on the server; E2E by construction |
| Room cap | 8 in a voice room (configurable) | Mesh upload cost |
| Text history | Client-side only (JSONL, per server/room) | Server stores no user data |
| Auth | Shared password, SCRAM-SHA-256 over Argon2id | Password never transmits; server stores only a verifier |
| Identity | Client-generated Ed25519 keypair, TOFU certificate pinning | Stable id and file-sender verification without accounts |
| NAT fallback | TURN relay (coturn), never persisted | 10–20% of peer pairs cannot connect directly |
| Server config | TOML (`config/server.toml`) | Rust-native; semantic match with the old YAML |
| Android shell | Thin Kotlin/Java shim for permissions, foreground service, SAF, MediaStore | Rust crates do not cover those platform APIs |
| Encode strategy | Encode once, fan the sample out to every peer | Avoids per-peer encoders; major CPU saving for screen share |

**Out of scope (same as the Kotlin v1):** iOS and web clients, server-side mixing/SFU,
screen-share audio, per-user roles/bans, server-side history, group E2E text encryption,
transfer resume, Wayland screen capture, Android screen capture.

---

## 2. Architecture

```
┌──────────────────────────────┐         ┌───────────────────────────────┐
│ Desktop / Android client     │         │ rust-chat-server (tokio)      │
│  UI (FerroUI/Slint/egui)     │◄───────►│  WSS :8443 (rustls)           │
│  client-core state machine   │  JSON   │  rooms · presence · text      │
│  webrtc-rs mesh (8 peers)    │  v2     │  SCRAM auth · rate limits     │
│  cpal/AAudio · Opus · VP8    │         │  opaque SDP/ICE relay         │
│  windows-capture · wgpu      │         │  TURN credential issuer       │
└───────────┬──────────────────┘         └───────────────┬───────────────┘
            │  WebRTC mesh: DTLS-SRTP (voice, screen)     │ HMAC TURN creds
            │  SCTP data channels (files, control)        ▼
            └──────────► peer ◄──────────► coturn relay (unchanged)
```

**Invariants**

1. The server never carries media and never stores file bytes.
2. The server is authoritative for room membership and text ordering.
3. Peer media/data is encrypted by DTLS-SRTP; a relayed connection stays end-to-end encrypted.
4. One failed peer degrades that peer only.

---

## 3. Workspace

```
crates/protocol        Envelope, DTOs, errors, limits, version
crates/crypto          Argon2id verifier, SCRAM-SHA-256, fingerprints
crates/server          axum WS, sessions, rooms, config, TLS, TURN, CLI
crates/client-core     State machine, reconnect, store, history, file transfer
crates/media           MediaEngine trait, mesh, audio/video pipelines (no platform code)
crates/media-desktop   cpal/WASAPI, windows-capture, SendInput, wgpu
crates/media-android   AAudio/oboe, platform AEC, GLES/Vulkan
crates/ui              Shared screens, widgets, theme
crates/app-desktop     rust-chat binary
crates/app-android     cdylib + thin Kotlin shell
xtask                  Packaging, manifests, version
spikes/ferroui         M0 UI gate (standalone Cargo project)
spikes/webrtc-loopback M0 media gate (standalone Cargo project)
deploy/                Docker, systemd, coturn, VPS scripts
docs/                  PLAN, PROTOCOL, BASELINE, SETTINGS, HISTORY, UPDATES
```

Dependency rule: `app-* → ui → client-core → protocol`; `server → protocol + crypto`;
`client-core` never imports a media backend. This mirrors the Kotlin `MediaEngine` seam that
kept the JVM and Android implementations swappable.

---

## 4. Protocol v2 (summary)

Normative text in [`PROTOCOL.md`](./PROTOCOL.md).

- One WSS connection, JSON envelopes `{v:2, type, id?, replyTo?, payload}`.
- `hello → challenge → auth → welcome`, then rooms/presence/chat/signaling.
- SCRAM-SHA-256 over Argon2id, identical math to v1; the verifier file format is byte-compatible,
  so an existing server's `config/verifier` can be reused.
- Opaque `rtc.signal` (`offer`/`answer`/`ice`/`bye`) relayed only inside a room.
- One designated offerer (`mySessionId < theirSessionId`). Both media slots are negotiated up
  front, so sharing toggles capture and never renegotiates.
- Two reliable data channels created by the offerer: `files` then `control`.
- Files: metadata via the server, bytes over SCTP, 64 KiB chunks, 4 MiB high-water, SHA-256,
  `.part` rename, cancel, no resume. Thumbnail JPEG framed with `RCPV`.
- Screen control: versioned postcard frames over `control`, lease enforced by the host.
- Same error code set and limits as v1, minus legacy capabilities and version-1 compatibility.

---

## 5. Media stack

| Concern | Choice |
|---|---|
| ICE/DTLS/SRTP/SCTP/SDP | `webrtc-rs` 0.21 / `rtc` 0.21 (pure Rust) |
| Audio I/O | `cpal` 0.18 (WASAPI, AAudio, CoreAudio, ALSA) |
| Echo cancellation | `webrtc-audio-processing` 2.1, with our own mixed playout as the far-end reference |
| Opus | `opus` 0.4, 48 kHz mono, 32 kbps, 20 ms, FEC/DTX |
| Jitter buffer / PLC | small custom reorder buffer + Opus PLC |
| Screen capture | `windows-capture` 2.0 (WGC); X11 later; Wayland unsupported |
| Video codec | VP8 via libvpx (royalty-free, matches v1, browser-compatible later) |
| Video render | `wgpu` YUV→RGB shader into the UI texture |
| Remote input | `windows` crate `SendInput` |
| TLS | `rustls` + `rcgen` self-signed, TOFU pinning via a custom certificate verifier |

Pipeline:

- **Voice:** mic 48 kHz → APM (AEC/NS/AGC) → Opus 32 kbps → one encoded frame → every peer.
  Receive → jitter buffer → Opus decode (PLC) → mix (per-peer gain/mute) → playout.
- **Screen:** WGC capture → scale → I420 → VP8 encode at the configured size/fps → one encoded
  frame → every peer. Receive → VP8 decode → texture. Keyframes on track open and PLI.
- **Mute** stops packets (it does not send silence). **Levels** are RMS of the local mic and each
  decoded remote stream.
- **Device switching** rebuilds the audio streams without touching peer connections.

**Media risk gate (M3).** Two headless peers must exchange Opus both ways and a data-channel
payload over loopback, then a 720p10 VP8 share, with acceptable echo cancellation and CPU. If
that fails, the contract allows falling back to libwebrtc bindings behind the same trait, but the
plan does not assume it.

---

## 6. Android

- `android-activity` + `cdylib`; UI from the shared toolkit.
- Thin Kotlin/Java shell for: microphone permission, call foreground service + notification,
  SAF file picker, MediaStore downloads, APK installer intent.
- Mic/playout: AAudio via cpal; platform AEC if an audio session is required, otherwise APM.
- Video: receive-only, VP8 software decode → GLES/Vulkan texture. No capture, no MediaProjection.
- Files: send through a SAF `content://` URI; receive into Downloads via MediaStore.
- Packaging: Gradle wrapper around `cargo-ndk` (low risk); test on the real TECNO LJ8, not only
  the emulator.
- The `screen-control` capability is never advertised by Android.

---

## 7. Feature parity checklist

Every row must exist or be explicitly dropped.

| Area | Items |
|---|---|
| Connection | host/port/TLS, display name, password, remember password in OS key store, auto-connect, saved server, forget server, pinned key copy/forget, TOFU approve + changed-key hard block, classified failure hints, reconnect with backoff |
| Rooms | list with counts/cap, join/leave (session stays), lobby, create (name/topic/voice), delete empty client rooms, config rooms not deletable, last-room memory, 8-member voice cap |
| Chat | feed, grouping, timestamps, 12/24h clock, text scale, Enter/Shift+Enter, 4 KiB cap, optimistic send + reconcile, local history per server/room (500 window, compaction, clear room/all, keep toggle) |
| Presence | self-mute, self-deaf, presenting, speaking dots, relayed marker, failed marker, join/left deltas, authoritative snapshots |
| Voice | muted-on-join, mic/speaker mute, gains, device pickers, live switching, levels, per-peer state, no-STUN warning, TURN credentials |
| Screen share | monitor/window picker, 720p/1080p/1440p + 5/10/15/30 fps defaults, inline pane, detached window, fullscreen, presenter banner, resolution display |
| Remote control | request/consent, grant/deny/revoke, 30 min lease (5–60), countdown, heartbeat, Windows host, screen shares only, Alt+F4 filter, release all on end |
| Files | send with caption, offer/accept/decline, progress, cancel, SHA-256 verify, `.part` rename, numeric suffix, downloads folder + writability check, open-when-finished, image preview, 8 GiB/255-char caps |
| Settings | Connection, Audio & Video, Chat, Files, Updates, Appearance, About; immediate apply; confirm destructive actions; copy diagnostics; theme system/light/dark; density comfortable/compact |
| Updates | `latest.json`, numeric compare, check at startup/now, skip version, SHA-256 verify, Windows zip swap with rollback + pending marker, Android APK installer, read-only fallback |
| Server ops | `init`, `hash-password`, `fingerprint`, `serve`; config; TLS on/off; Docker image; healthcheck; systemd unit; VPS install/update scripts; coturn; graceful shutdown; rate limits; no stored messages/files |

---

## 8. Milestones

| # | Deliverable | Gate | Est. |
|---|---|---|---|
| **M0** | Baseline, workspace/CI, docs, FerroUI gate, media gate skeleton | UI decision; both spikes measured | 1 wk |
| **M1** | `protocol` + `crypto` + server: auth, TLS, rooms, chat, presence, CLI, config, Docker | 2 clients chat over real WSS; wrong password/rate limit; pin; server ≤ 30 MB | 2 wk |
| **M2** | Desktop shell: connect/trust, rooms, chat, history, settings, keyring, update check | 2 clients chat; restart restores room/history; idle RSS measured | 3 wk |
| **M3** | Media spike → mesh: 2-peer voice, Opus, APM/AEC, data channel, then 4-peer + TURN | Voice both ways; AEC usable; CPU/RAM measured; go/no-go | 2–3 wk |
| **M4** | Mesh polish: presence/levels/stats/ICE restart/reconnect | 4 peers; churn; relay path; no stale peers | 2 wk |
| **M5** | Screen share + remote control | Share window/screen; repeated start/stop; input with lease; CPU target | 3 wk |
| **M6** | File transfer + previews + captions | 1 GiB hash match; cancel mid-flight; relay path; no resume documented | 1.5 wk |
| **M7** | Android: UI, audio, video receive, files, foreground service, IME | Real TECNO device: voice + text + receive + watch + settings persist | 4–6 wk |
| **M8** | Parity polish: settings, updater, installers, CI matrix, soak, docs | 1 h soak with 4 clients; flat memory; release artifacts | 3 wk |

Total ~18–22 weeks full-time; realistically 6–9 months part-time. M3/M5 and M7 are where it can
slip.

---

## 9. Testing and CI

- **Unit:** envelope/DTO round-trips, SCRAM vectors, verifier, room state machine, rate limiter,
  filename sanitizer, history compaction, control-frame codec.
- **Integration:** real server on an ephemeral port + N in-process WSS clients; TLS pin
  accept/reject; reconnect after restart; room moves; 4-client mesh; 2 MiB file hash; control
  frames.
- **Media loopback (headless):** two engines, synthetic audio/video, `iceServers = []`, assert
  bytes both ways, keyframes, start/stop share, data-channel integrity.
- **Soak:** 4 clients, 1 h, sample RSS/CPU; fail on monotonic growth.
- **CI:** Windows + Linux + macOS client, Linux amd64/arm64 server; `fmt --check`,
  `clippy -D warnings`, `test`, `cargo audit`; Android emulator smoke in M7.
- **Perf gate:** startup, idle RSS, voice CPU, share CPU — committed numbers per release.

---

## 10. Risks

| Risk | Mitigation |
|---|---|
| `webrtc-rs` maturity (self-interop only) | M3 gate; media behind a trait; documented libwebrtc fallback |
| Echo cancellation quality | APM with our own playout reference; headset default; PTT option; real-hardware test |
| FerroUI maturity (7 weeks old, one author, emulator-only Android) | Timeboxed gate with hard pass/fail; fallback Slint (licensing decision) or egui |
| Android platform glue | Thin Kotlin shell; real device from M7; no emulator-only claims |
| VP8 encode CPU on desktop; decode on phone | Encode once/fan out; 720p10 cap; hardware MediaCodec later |
| Scope | Vertical slices + gates; desktop parity before Android |
| Licensing | MIT FerroUI; libvpx/Opus/webrtc-rs permissive; avoid openh264; Slint only with an explicit license choice |
| Updater safety | SHA-256, staging, rollback helper, pending marker, zip path checks |
| Protocol redesign regressions | v1 semantics kept; conformance tests derived from the Kotlin spec |

---

## 11. The M0 toolkit gate (FerroUI)

FerroUI 0.5.0 is the candidate for one UI codebase across desktop and Android. The project is
MIT, seven weeks old, effectively single-maintainer, pre-1.0 (0.6 may change the API), and its
Android support is verified on an emulator only. No RSS figure has been published; expected UI
cost is Skia-class (roughly 100–200 MB idle).

On Windows the default renderer is Skia + ANGLE, whose build needs **libclang** (bindgen); a clean
machine fails at `mozangle` until LLVM is installed. FerroUI also ships an opt-in native Windows
backend (Direct2D/DirectWrite/D3D11) that needs no Skia, ANGLE or libclang and is the variant this
gate measures first. Both variants are in `spikes/`.

The gate passes only if all of these hold:

1. Builds and runs on Windows from crates.io (no fork, no patch).
2. A 5,000-message virtualized list scrolls at 60 fps.
3. Multiline CJK/emoji input works, including IME and selection.
4. A fake 720p10 texture presents at <10% CPU.
5. Android emulator **and** the real TECNO LJ8 build and run with working IME, back and rotation.
6. Idle RSS ≤ 180 MB; compile time and binary size recorded.
7. No framework bug forces a fork or a product-level workaround.

If any gate fails: **Slint 1.19** (mature, one codebase, GPLv3 or paid/royalty-free — licensing
decision required) is the pre-agreed fallback; egui is the fallback if only desktop matters.

---

## 12. M0 results

| Item | Result |
|---|---|
| Kotlin baseline | 408–420 MB working set, 542–554 MB private, 79 threads, ~9% of one core idle (connect screen) |
| Workspace | 11 crates; `cargo check`/`test` green (26 tests) |
| Protocol crate | Envelope v2, DTOs, error codes, limits, round-trip tests |
| Crypto crate | Argon2id verifier (Kotlin-file-compatible), SCRAM-SHA-256, fingerprints, tests |
| Server CLI | `init`, `hash-password`, `fingerprint` work; ECDSA P-256 cert + TOFU fingerprint printed |
| Docs | `PLAN.md`, `PROTOCOL.md`, `BASELINE.md`, this file |
| FerroUI gate | see `spikes/ferroui/README.md` and `docs/BASELINE.md` |
| Media gate | **passed**: two `webrtc-rs` peers connect over loopback and a data-channel payload arrives intact; see `spikes/webrtc-loopback/README.md` |
