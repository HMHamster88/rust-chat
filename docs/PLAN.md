# rust-chat — Design Plan

A self-hosted chat with text, voice, screen sharing, remote control and peer-to-peer file
transfer, rewritten in Rust from `../kotlin-chat`.

The Kotlin project is the **feature specification**, not the compatibility target. No Kotlin
code, module layout, library choice or wire protocol is preserved. Protocol v2 is defined in
[`PROTOCOL.md`](./PROTOCOL.md).

- **Server:** Rust (tokio + axum + rustls), one static binary.
- **Clients:** Rust — desktop first (Windows, then Linux/macOS) and Android.
- **Media:** pure Rust WebRTC (`webrtc-rs`/`rtc`), Opus, VP8, WebRTC audio processing.
- **UI:** FerroUI 0.5 (Direct2D on Windows, Skia on Android), decided by the M0 gates; texture
  presentation is the first M3 gate.
- **Status:** M5 remote control complete (control data channel, consent, 30-minute lease,
  SendInput, Alt+F4 filter, release-all). Next: screen-share picker/resolution polish, then M6
  files. M0–M4 results and the M5 details are in §11/§12.

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
│  UI (FerroUI/Slint/egui)     │◄───────►│  WSS :8444 (rustls)           │
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
spikes/ferroui-d2d     M0 Direct2D variant of the UI gate
spikes/ferroui-android M0 Android gate (cdylib + Gradle shell)
spikes/ferroui-texture M3 Direct2D live-texture gate
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
| Video render | FerroUI `TexturePresenter` over Direct2D/D3D11 shared textures on Windows; Skia/GLES on Android; `WriteableBitmap` + `Image` as the first-slice/fallback path |
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

- Shared Rust UI and core (`ferroui-android` for the M0 candidate); `cdylib` built with `cargo ndk`.
- FerroUI already ships the Java layer this design calls for: `FerroApplication` (loads the native
  library named in the manifest meta-data), `FerroMainActivity`, `FerroView`, and the input
  connection. The M0 spike uses it unchanged; our own Java additions (SAF picker, MediaStore,
  call foreground service, APK installer intent) slot in beside it.
- Mic/playout: AAudio via cpal; platform AEC if an audio session is required, otherwise APM.
- Video: receive-only, VP8 software decode → GLES/Vulkan texture. No capture, no MediaProjection.
- Files: send through a SAF `content://` URI; receive into Downloads via MediaStore.
- Packaging: Gradle (AGP 8.13.2, compileSdk 35, minSdk 26) around `cargo ndk`; release `.so`
  stripped, debug builds are not representative of size.
- Test on the real TECNO LJ8, not only the emulator.
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

**Verdict (M0): adopt FerroUI.** It passed the desktop gates (133–152 MB with a 5,000-item list,
virtualization confirmed, ~0% idle CPU) and the Android gates (51.4 MB APK, installs/launches on
the TECNO, 5,000-item list virtualizes, keyboard/Back/rotation work, 175–186 MB PSS). Use the
native **Direct2D** backend on Windows (no Skia/ANGLE/libclang, lower memory) and **Skia** on
Android (the only backend there). The two remaining unknowns are explicit M3 tasks:

1. **Live video texture** — *pre-work done 2026-11-25*: `spikes/ferroui-texture` presents
   1280×720 BGRA frames through `TexturePresenter` + `D3D11TextureImporter`, **3.0% of one core
   at 10 fps** in release (peak 5.7%), 81 MB working set, **0 explicit readbacks**, and a pixel
   capture confirms live motion. Remaining: the VP8→I420→BGRA→D3D11 adapter, decode on a worker
   thread, and a lease-aware texture pool (the spike uses an eight-texture rotation).
2. **Framework maturity**: pin `=0.5.0`, keep UI code behind our own screens/widgets, and do not
   depend on internal control templates.

---

## 12. Milestone results

| Item | Result |
|---|---|
| Kotlin baseline | 408–420 MB working set, 542–554 MB private, 79 threads, ~9% of one core idle (connect screen) |
| Workspace | 11 crates; `cargo check`/`test` green (26 tests) |
| Protocol crate | Envelope v2, DTOs, error codes, limits, round-trip tests |
| Crypto crate | Argon2id verifier (Kotlin-file-compatible), SCRAM-SHA-256, fingerprints, tests |
| Server CLI | `init`, `hash-password`, `fingerprint` work; ECDSA P-256 cert + TOFU fingerprint printed |
| Docs | `PLAN.md`, `PROTOCOL.md`, `BASELINE.md`, this file |
| FerroUI gate | builds on Windows (Skia needs the documented LLVM/libclang prerequisite; Direct2D needs only MSVC). 5,000-item list: **133.3 MB** working set on Direct2D, **152.3 MB** on Skia; 50,000 items cost +8 MB, so the list virtualizes. Idle CPU ~0%. Android: builds with `cargo ndk`, **51.4 MB APK** after stripping, installs and runs on the TECNO; 5,000-item list virtualizes (10–16 realized items); IME connects and Back hides it; rotation works; **175–186 MB PSS** on a fresh launch |
| Media gate | **passed**: two `webrtc-rs` peers connect over loopback and a data-channel payload arrives intact; see `spikes/webrtc-loopback/README.md` |

### M1 — WebSocket control plane (2026-11-25)

- WSS (rustls/ring) with the certificate from `init` and a `/healthz` endpoint; TLS can be turned
  off for LAN development. Verified with `curl -k` and a Node WSS client (`hello → challenge`,
  Argon2id `m=65536`).
- SCRAM-SHA-256 handshake against the verifier; failed attempts are gated per IP (3/min → 30 s
  lockout, successful logins do not consume attempts); duplicate names get ` (2)`; the
  `serverSignature` proves the shared password to the client.
- Rooms: config rooms plus client-created rooms persisted in `rooms.json`, an implicit lobby, the
  voice cap, create/delete rules, authoritative `room.state` snapshots and deltas, and
  `room.flags`.
- Chat, presence, opaque `rtc.signal` relay (same room only), file metadata relay, ping/pong and
  `server.shutdown`.
- Per-session token buckets (chat 5/s, signaling 50/s, offers 20/min), bounded outbound queues;
  a slow consumer is closed instead of growing memory.
- Tests: 17 server unit tests + 5 real WebSocket integration tests (wrong password, two clients
  chatting, signal relay and bad target, room create/join/leave/delete, `no-auto-room`), 49
  workspace tests total; `clippy -D warnings` clean.

### M2 — desktop shell (2026-11-25)

- `client-core`: flat JSON settings (default port 8444), Windows key store for the optional saved
  password, TOFU pin store, rustls with `ring`, SCRAM handshake, automatic reconnect
  (0.5 s → 30 s with jitter), rooms, chat, presence, and a JSONL history per server/room with a
  500-message window and compaction.
- FerroUI Direct2D shell: connect screen (host, port, name, password, TLS, remember), a trust
  prompt that shows the fingerprint and requires Approve/Deny, a **Forget pinned key** action,
  room list, members, chat feed and composer. The screen follows the connection/room state.
- Verified by UI Automation against a real server: connect, render `# Lobby` and members, send
  `hello from the UI`; after killing and relaunching the app the message is restored from
  history (`HISTORY-OK`).
- TLS pinning test against a real TLS server: first contact prompts and pins the certificate,
  the next connection is silent, and a wrong pin is a hard `certificate changed` failure.
- Memory/CPU: **99.4 MB** working set / 0.2% idle on the connect screen; **98.3 MB** / 0.6% in a
  room (the Kotlin client: 408–420 MB / ~9%).
- Remaining M2 polish (not gating): a full settings screen, theme/density, update checker,
  room create/delete/leave controls in the UI, and audio-device selection (M3).

### M3 — voice (2026-11-25)

- `crates/media`: Opus 48 kHz mono at 32 kbps (complexity 5), a reordering jitter buffer with a
  bounded gap, and a `VoicePeer` that wraps one `webrtc-rs` connection with an audio track plus a
  host-candidate SDP offer/answer. SSRC and payload type are read back after negotiation.
- `crates/media-desktop`: cpal/WASAPI capture and playout with format/sample-rate conversion,
  shared 10 ms APM frames, and a 1 s playout queue. **Sonora 0.3**, the pure-Rust port of WebRTC
  APM, supplies AEC3, noise suppression and AGC2 without a C++/meson toolchain.
- `crates/app-desktop`: one peer per member with `voice = true`; deterministic offerer (smaller
  session id offers, the other answers) removes glare. Muted on join; mic/speaker mute and live
  levels; per-member `[voice]` and `♪` markers. SDP carries host candidates inline, so no
  trickle ICE is needed on a LAN.
- Protocol: `Member`/`PresenceUpdate`/`MemberUpdated` carry a `voice` flag, so the mesh knows
  exactly who is in the call without a second subscription.
- Tests: a headless two-peer WebRTC test carries Opus both ways, and a full integration test
  connects two SCRAM clients through the real server, relays the offer/answer, and receives
  non-silent audio on both sides (0.5 s). The whole workspace test suite and
  `clippy -D warnings` are clean.
- Verified through the real desktop UI with two instances: both show `voice: live` and the two
  members as `[voice]`, and the call survives a WASAPI xrun (xruns no longer end the session).
  One client in a call measures **114.6 MB** working set and **1.9%** of one core.

### M3 — screen sharing (2026-11-25)

- **Codec choice.** The plan named VP8 through libvpx. On Windows that requires a system libvpx
  (vcpkg/pkg-config, NASM and Perl), and no maintained Rust decoder binding exists. OpenH264 was
  considered as the low-friction alternative, but H.264 is patent-encumbered while VP8 is
  royalty-free. The implemented path is **pure-Rust VP8** (`vp8-encoder` / `vp8-decoder` 0.1.3):
  no libvpx, no NASM, no DLLs, and it cross-compiles to Android.
- **Capture.** Windows Graphics Capture (monitor 1) delivers BGRA at up to 10 fps (throttled in
  the handler). The frame is box-downscaled to at most 1280×720, converted to I420 (BT.601) and
  encoded at 1.5 Mbps; the first frame and every 5 s are keyframes.
- **Transport.** The same peer connection carries Opus and a VP8 `sendrecv` track. One encode is
  broadcast to every peer. The receiver strips the VP8 payload descriptor
  (`rtc`'s `Vp8Packet`), reassembles on the RTP marker bit, decodes to I420 and converts to BGRA
  on a worker.
- **Presentation.** BGRA is uploaded with `UpdateSubresource` into a ring of D3D11 textures,
  published as a `FrameLease` and imported by FerroUI's `TexturePresenter` — the zero-readback
  path from the texture spike.
- **UI.** A **Share screen** button in the voice bar (toggle), a presenter pane that appears while
  someone is `[presenting]`, and a status line that reports `receiving screen 1280x720`.
- **Benchmark (release, 1080p→720p synthetic desktop).** Encode **14.1 ms/frame** average
  (53 ms first keyframe), decode **3.6 ms/frame**, **1.59 Mbps** actual at 10 fps; scale+I420
  conversion 11.4 ms/frame.
- **Footprint.** A 4K monitor sender at 720p10: **133.4 MB** working set / 184.7 MB private /
  **13.4%** of one core average (27% peak).
- Tests: a two-peer WebRTC test carries VP8 frames and decodes a non-flat 640×360 picture; the
  two-peer audio test still passes on the same connection. An opt-in capture test proves WGC
  delivers a 3840×2160 BGRA frame. The UI smoke test shows the shared desktop in the receiver's
  presenter pane.
- Still open for M4: TURN/relay path, ICE restart, PLI-driven keyframes, monitor/window picker,
  resolution/fps settings, 4-peer measurement.

### M4 — mesh polish (2026-11-25)

- **TURN.** `welcome.turn` is now kept in the client snapshot and translated into a WebRTC
  `RTCIceServer`, so any configured coturn is used for srflx/relay candidates. Without TURN the
  mesh stays on host candidates.
- **Peer lifecycle.** Every peer task reports completion on a controller channel; a dead peer is
  removed immediately, so a stale entry can never block a reconnect. The designated offerer
  rebuilds the connection with 0.5/1/2 s backoff, up to three attempts; the other side waits for
  the new offer. `RTCPeerConnectionState::Failed/Disconnected` is detected within 1 s.
- **Keyframes.** A receiver asks for a keyframe when the VP8 decoder loses its reference (and once
  after connecting) using `rtc.signal {kind:"need-keyframe"}`. The sharer forces its next encoded
  frame to be a keyframe; the 5 s periodic keyframe remains the fallback.
- **Stats.** Every 2 s each peer reports the selected candidate pair's RTT and whether either end
  is a relay candidate. The member line shows `(12 ms)` or `(relay 20 ms)`.
- **Reconnect.** Losing the control plane stops the mesh and arms an auto-rejoin; once the client
  is back in the same room it rebuilds the call without user action. Verified by killing and
  restarting the server during a live call.
- Verified with **4 real app instances**: all show three peers, `(0 ms)` links, 117.8 MB working
  set and 2.2% of one core on the measured client. A 4-client integration test joins the mesh,
  carries audio from all peers, then drops one participant and checks the other three keep
  receiving audio.
- Tests: 78 total (TURN credentials, 4-peer churn, keyframe plumbing), `clippy -D warnings` clean.
- Remaining for M5+: live coturn relay test, monitor/window picker, resolution/fps settings,
  audio device pickers, remote-control lease.

### M5 — remote control (2026-11-25)

- **Transport.** The offerer creates a reliable, ordered `control` data channel before its offer;
  the answerer adopts it in `on_data_channel`. Messages are small JSON objects:
  `request`, `grant`, `deny`, `revoke`, `heartbeat`, `end`, and `input`.
- **Consent and lease.** A viewer can request control only while someone else is presenting. The
  host sees the requester's name and a 30-minute default (clamped to 5–60), then Allow or Deny.
  Grants are one-controller-at-a-time. The viewer sends a heartbeat every 5 s; the host revokes
  after 15 s of silence or when the lease expires, and either side can stop immediately.
- **Input.** The viewer maps pointer, wheel and keyboard events inside the presenter to an
  `input` message. The host injects them with `SendInput`: absolute mouse movement across the
  virtual desktop, left/right/middle buttons, vertical/horizontal wheel, and virtual-key presses
  derived from the WPF-style `Key` plus `VkKeyScanW` for printable characters. Alt+F4 is filtered;
  all held keys and buttons are released on lease end, revoke, peer loss or shutdown.
- **UI.** `Request control` on the viewer of a share, `Allow 30 min`/`Deny` on the host, a
  countdown (`controlling X — 29:56` / `X is controlling — 29:56`), and `Stop control`.
- **Verified with two real app instances** using a dry-run injector on the host: pointer moves, a
  click and an `A` keypress all arrived over the data channel and mapped to the shared monitor
  (the log showed `move 1920,1077`, `button Left true/false`, `key 0x41 true/false`).
- Tests: data-channel round trip, control message schema, injector mapping, Alt+F4 filtering and
  release-all. 84 workspace tests; `clippy -D warnings` clean.
- Remaining: monitor/window picker, resolution/fps settings, repeated start/stop soak, and a live
  (non-dry-run) input check on a second machine.
- Still open for the rest of M3: VP8 screen capture/encode/decode and the Direct2D presenter,
  device pickers, TURN, and the 4-peer measurement.
