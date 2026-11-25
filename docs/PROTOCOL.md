# rust-chat — Wire Protocol v2

Normative reference for the control channel. Media negotiation is carried opaquely inside
`rtc.signal`; this document does not constrain SDP. The Kotlin `PROTOCOL.md` is the feature
reference; v2 keeps its semantics and drops the legacy compatibility hacks.

- Transport: **WSS** (TLS 1.2/1.3 via rustls), one connection per client, text frames.
- Encoding: JSON, UTF-8.
- Keys are `camelCase`.
- Timestamps are epoch milliseconds; anything the server stamps uses the server clock.
- A v2 server does **not** accept v1 clients. The version is refused with `E_PROTO_VERSION`.

---

## 1. Envelope

Every frame is one JSON object:

```json
{ "v": 2, "type": "chat.send", "id": "c-17", "payload": { "text": "hi", "clientMsgId": "c-17" } }
```

| Field | Type | Notes |
|---|---|---|
| `v` | int | Must be `2`; anything else gets `E_PROTO_VERSION` and a close. |
| `type` | string | `namespace.action`. |
| `id` | string? | Client correlation id, echoed as `replyTo` in direct replies. |
| `replyTo` | string? | Set by the server on a direct reply. |
| `payload` | object | Type-specific body; `{}` when empty. |

Maximum frame: **64 KiB**. Unknown `type` values are ignored (forward compatibility). Unknown
fields inside a known payload are rejected with `E_BAD_PAYLOAD`.

---

## 2. Session lifecycle

```
client                                          server
  │  hello {protocol, name, pubkey, features}      │
  ├───────────────────────────────────────────────►│
  │  challenge {nonce, kdf}                        │
  │◄───────────────────────────────────────────────┤
  │  auth {clientNonce, proof}                     │
  ├───────────────────────────────────────────────►│
  │  welcome {sessionId, rooms, turn, serverSig}   │
  │◄───────────────────────────────────────────────┤
  │  room.state (lobby or none)                    │
  │◄───────────────────────────────────────────────┤
```

Any failure before `welcome` is answered with `error`, then a normal WebSocket close.

### 2.1 `hello` (C→S)

```json
{
  "protocol": 2,
  "name": "alice",
  "pubkey": "<base64 Ed25519 public key>",
  "features": ["video", "file-preview", "screen-control"]
}
```

- `name`: 1–32 characters after trimming, no control characters, no leading/trailing space.
  A collision is resolved by the server appending ` (2)`; it is never rejected.
- `pubkey` is required and identifies the client across reconnects (`pubkeyFp` in members).
- `features` is advisory and used by the UI, not for offer/answer role selection (see §6):
  - `video` — can capture and send a screen share.
  - `file-preview` — understands the thumbnail frame before a file offer is accepted.
  - `screen-control` — can inject remote input (Windows hosts) or request it.

### 2.2 `challenge` (S→C)

```json
{ "nonce": "<base64, 16 bytes>",
  "kdf": { "alg": "argon2id", "salt": "<base64>", "m": 65536, "t": 3, "p": 1 } }
```

`salt` is stable for the life of the verifier, so clients may cache the derived key for the
session.

### 2.3 `auth` (C→S)

```
authKey       = Argon2id(password, salt, m, t, p)          // 32 bytes
clientKey     = HMAC-SHA256(authKey, "client key")
storedKey     = SHA-256(clientKey)
serverKey     = HMAC-SHA256(authKey, "server key")

authMessage   = UTF8(name) ‖ 0x00 ‖ serverNonce ‖ 0x00 ‖ clientNonce
clientSig     = HMAC-SHA256(storedKey, authMessage)
clientProof   = clientKey XOR clientSig
serverSig     = HMAC-SHA256(serverKey, authMessage)
```

```json
{ "clientNonce": "<base64, 16 bytes>", "proof": "<base64, 32 bytes>" }
```

The server verifies
`SHA-256(clientProof XOR HMAC(storedKey, authMessage)) == storedKey` in constant time.
On success `welcome` carries `serverSignature`, which the client **must** verify and abort on
mismatch. That is what detects a substituted endpoint.

### 2.4 `welcome` (S→C)

```json
{
  "sessionId": "s_7f3a…",
  "serverName": "home",
  "motd": "hi",
  "protocol": 2,
  "serverSignature": "<base64, 32 bytes>",
  "rooms": [
    { "id": "lobby", "name": "Lobby", "topic": "", "memberCount": 2, "cap": 8 }
  ],
  "turn": {
    "urls": ["turn:host:3478?transport=udp", "stun:host:3478"],
    "username": "1763…:s_7f3a",
    "credential": "…",
    "ttlSeconds": 3600
  },
  "limits": { "maxChatBytes": 4096, "maxMembersPerRoom": 8 }
}
```

`turn` is absent when no relay is configured. Clients then use host/srflx candidates only and
show the "no STUN/TURN" warning.

A client that sends `features: ["no-auto-room"]` is authenticated but starts outside every room;
it receives `welcome` and no `room.state` until it sends `room.join`. This is how the connect
screen stays usable without joining.

---

## 3. Client → server

| Type | Payload | Effect |
|---|---|---|
| `room.list` | `{}` | Answers `room.list.result`. |
| `room.join` | `{roomId}` | Leaves the current room and joins `roomId`; answers `room.state`. |
| `room.leave` | `{}` | Leaves the room, keeps the session; answers `room.left`. |
| `room.create` | `{name, topic?, voice?}` | Creates a room, moves the creator into it, broadcasts `room.list.result`, answers `room.state`. |
| `room.delete` | `{roomId}` | Removes an empty client-created room; broadcasts `room.deleted` and the new list. |
| `chat.send` | `{text, clientMsgId}` | Relayed to the room as `chat.message`. |
| `presence.update` | `{selfMute?, selfDeaf?, sharing?, voice?}` | Broadcast as `member.updated`. |
| `rtc.signal` | `{to, signal}` | Relayed verbatim as `rtc.signal` to `to`. |
| `file.offer` | `{transferId, name, size, sha256}` | Relayed to the room as `file.offer`. |
| `file.accept` | `{transferId, to}` | Relayed to the sender only. |
| `file.decline` | `{transferId, to, reason?}` | Relayed to the sender only. |
| `file.cancel` | `{transferId, to?}` | Relayed; `to` omitted means the whole room. |
| `ping` | `{ts}` | Answers `pong`. |

`text` is trimmed, non-empty, and at most `limits.maxChatBytes` (4096 by default) UTF-8 bytes.
`clientMsgId` is echoed in `chat.message` so optimistic UI can reconcile.

`chat.send`, `presence.update`, `rtc.signal` and the file messages are only valid inside a room;
otherwise the server answers `E_NOT_IN_ROOM`.

### 3.1 `rtc.signal`

```json
{
  "to": "s_9a1c…",
  "signal": {
    "kind": "offer",
    "sdp": "v=0…"
  }
}
```

```json
{
  "to": "s_9a1c…",
  "signal": {
    "kind": "ice",
    "candidate": "candidate:…",
    "sdpMid": "0",
    "sdpMLineIndex": 0
  }
}
```

`kind` is `offer`, `answer`, `ice`, `bye` or `need-keyframe`. The server checks only that `to` is
a live session in the sender's room, and forwards SDP/candidates byte-for-byte. A target outside
the room is `E_BAD_TARGET`, without revealing whether the session exists. `need-keyframe` asks a
screen sharer to force its next VP8 frame to be a keyframe.

---

## 4. Server → client

| Type | Payload |
|---|---|
| `room.list.result` | `{rooms: [{id, name, topic, memberCount, cap}]}` |
| `room.state` | `{room, self, members}` — full authoritative snapshot |
| `room.left` | `{rooms}` — sent after `room.leave` |
| `room.deleted` | `{roomId}` |
| `room.flags` | `{deletable: [roomId, …]}` |
| `member.joined` | `{member}` |
| `member.left` | `{sessionId, reason}` |
| `member.updated` | `{sessionId, selfMute, selfDeaf, sharing, voice}` |
| `rtc.signal` | `{from, signal}` |
| `chat.message` | `{messageId, roomId, from, name, text, ts, clientMsgId?}` |
| `file.offer` | `{from, transferId, name, size, sha256}` |
| `file.accept` | `{from, transferId}` |
| `file.decline` | `{from, transferId, reason?}` |
| `file.cancel` | `{from, transferId}` |
| `error` | `{code, message, retryable, fatal}` |
| `pong` | `{ts, serverTs}` |
| `server.shutdown` | `{inMs, reason}` |

`member`:

```json
{
  "sessionId": "s_7f3a…",
  "name": "alice",
  "pubkeyFp": "SHA256:9c…",
  "joinedAt": 1763000000000,
  "selfMute": false,
  "selfDeaf": false,
  "sharing": false,
  "voice": false,
  "features": ["video"]
}
```

`room.state` is sent on join, on every membership change (in addition to the delta), and on
demand. Clients treat deltas as hints and the snapshot as truth.

---

## 5. Rooms and presence semantics

- A session is in exactly **one** room at a time; `room.join` implies leaving the previous one.
- `lobby` exists implicitly from config, is text-only in spirit, and is the post-auth default
  unless the client sent `no-auto-room`.
- Rooms come from `config/server.toml` plus client-created rooms when `allowClientRooms` is true,
  bounded by `maxRooms`. Created rooms are persisted in `config/rooms.json`, so `server.toml`
  stays hand-edited.
- A room id is a slug of its name plus a short random suffix. Names are unique case-insensitively.
- An empty client-created room may be removed with `room.delete`. Config rooms, the lobby and any
  room with members are refused. `room.flags` tells clients which rooms are currently removable.
- Voice rooms enforce `maxMembersPerRoom` (8) at join: `E_ROOM_FULL`. Text-only rooms are uncapped.
- `member.left` reasons: `leave`, `disconnect`, `kicked_timeout`, `server_shutdown`.
- Presence flags are advisory; the server does not verify that a client claiming `selfMute` is
  actually muted.
- `voice = true` means "this session is in the room's voice mesh". The desktop client sets it
  when the user joins a call, together with `selfMute = true` (muted on join), and clears it on
  leave. `sharing = true` means "this session is sending a screen share"; it is independent of
  the voice flag and is what shows the presenter.

**Ordering invariant.** `welcome` and the joining session's first `room.state` are enqueued while
the server holds the room lock, before any `member.joined`/`member.left`/`chat.message` for that
session. A client never receives a room delta it would have to apply before it knows its room, and
snapshots always dominate deltas.

---

## 6. Peer negotiation (mesh)

The server is unaware of this section; it only relays `rtc.signal`.

### 6.1 Topology

- One peer connection per remote member, created when the member appears in `room.state`.
- Tracks: one audio (mic) and one video (screen share) transceiver, negotiated up front.
- Data channels: the offerer creates the reliable, ordered `control` channel before its offer.
  The `files` channel is added in M6 with the same pattern.
- On `member.left`, close and remove the local peer connection immediately.
- Starting or stopping a share starts/stops the capture source. It never changes the SDP.

### 6.2 One designated offerer

Both sides derive the same role from the session ids:

```
offerer  = mySessionId < theirSessionId     (lexicographic)
answerer = mySessionId > theirSessionId
```

The offerer:

1. Creates an audio transceiver (`sendrecv`) and a video transceiver (`sendrecv`).
2. Creates the `files` and `control` data channels.
3. Sends the offer immediately.

The answerer waits, applies the offer, attaches its microphone **after**
`setRemoteDescription(offer)` and **before** `createAnswer()`, answers the audio slot
`sendrecv`, and answers the video slot:

- `sendrecv` if it can capture video (desktop), or
- `recvonly` if it only renders shares (Android).

Because the offerer creates both media slots up front, glare cannot occur and no rollback path
is needed. An offer arriving at the offerer, or an answer at the answerer, is a protocol error
and is dropped.

### 6.3 Media slots

Every peer therefore has an audio and a video slot even when nobody is presenting. The cost is
one idle m-line per peer. Adding a genuinely dynamic media path later (a lazily created channel,
a third track) would require perfect negotiation with rollback and replace this section.

### 6.4 ICE

- Servers come from `welcome.turn`; with no TURN, only host/srflx candidates exist.
- The desktop client passes those TURN REST credentials straight into its ICE configuration, so a
  configured coturn provides relay candidates without any protocol change.
- Trickle ICE via `rtc.signal {kind:"ice"}` is defined, but the desktop implementation gathers
  host candidates before sending the SDP, so offers and answers carry their candidates inline.
  The `ice` kind remains available for networks that need it.
- Keyframes: the sender emits one every 5 seconds (and on share start). PLI-driven keyframes are
  an M4 refinement.
- The UI exposes per-peer state: `new / connecting / direct / relayed / failed`. `relayed` is
  derived from the selected candidate pair, never guessed.
- The peer that owns the offer retries a failed connection with an ICE restart, up to three
  times with growing backoff.

### 6.5 Remote control

The `control` data channel carries newline-free JSON objects with a `t` tag:

| `t` | Body | Meaning |
|---|---|---|
| `request` | `{leaseSecs}` | Ask the sharer for control. |
| `grant` | `{leaseSecs}` | Accept; both sides start the lease. |
| `deny` | `{reason}` | Refuse. |
| `revoke` | `{reason}` | Host ends an active grant. |
| `end` | `{reason}` | Viewer ends control. |
| `heartbeat` | — | Viewer liveness, every 5 s. |
| `input` | `{event}` | One mouse/key event. |

`event` is `{kind:"mouse_move", x, y}` in 0..1 coordinates of the shared source (monitor or
window), `{kind:"mouse_button", button, pressed}`, `{kind:"wheel", delta, horizontal?}`, or
`{kind:"key", vk, pressed}`.

- A request is accepted only while the host is sharing and no other grant is active.
- Leases are clamped to 5–60 minutes (30 default); both ends enforce expiry, and the host also
  revokes after 15 s without a heartbeat.
- Alt+F4 is filtered on the host; all held keys/buttons are released when control ends by any
  path. Control never affects the host outside an active lease.

---

## 7. Data channels

### 7.1 File transfer

```
sender                              receiver
  │ file.offer (via server, metadata only)
  ├─────────────────────────────────►│
  │            file.accept (via server)
  │◄─────────────────────────────────┤
  │  "files" channel
  │  64 KiB chunks ──────────────────►│  SCTP backpressure at 4 MiB
  │                                  │  SHA-256 → rename .part
```

- `transferId` is generated by the sender and unique per sender. It is reused for every receiver
  that accepts the same offer; rows are distinguished by `(direction, transferId, peerId)`.
- The receiver sends `file.accept` before the sender writes bytes.
- **One transfer at a time per peer.** Chunks carry no transfer header.
- Chunks are 64 KiB. The sender suspends while `bufferedAmount` exceeds 4 MiB and resumes on the
  channel's buffered-amount callback.
- The receiver writes to `.<name>.part`, hashes while writing, and renames only on a matching
  SHA-256. A mismatch deletes the partial file and fails the transfer.
- Incoming files go to the user's Downloads folder, with a numeric suffix on collision.
- Either side may `file.cancel`; the receiver deletes the partial file. Declines go to the sender
  only, with an optional `reason`.
- No resume in v2: a reconnect cancels transfers in flight.
- Server caps: 8 GiB per file, 255-character names, 20 offers/min per session.

**Thumbnail before accept.** An image offer may send a small JPEG on the `files` channel before
the receiver accepts, framed so it cannot be confused with file bytes:

```
magic   4 bytes   "RCPV"
kind    u8        0x01 = thumbnail
length  u32 BE    JPEG byte length
bytes   [u8; length]
```

Only peers advertising `file-preview` receive it.

**Captions** are a client convention, not an offer field: send a normal `chat.send` with
`clientMsgId = "file-caption:<transferId>"` and the caption as text. Newer clients attach it to
the file bubble; older ones show a normal message.

### 7.2 Screen control

Desktop-only, peer-to-peer, never relayed by the server. The viewer requests control, the
presenter consents, and only then does the viewer send input. One controller at a time.

Frames travel on the `control` channel, versioned and length-prefixed:

```
u8 version = 1
u32 BE length
[length] postcard-encoded ControlFrame
```

```rust
enum ControlFrame {
    Request,
    Grant { lease_seconds: u32 },
    Deny { reason: String },
    Revoke { reason: String },
    Cancel,
    Heartbeat,
    PointerMove { x: f32, y: f32 },              // normalized 0..1 inside the shared screen
    PointerButton { button: MouseButton, down: bool },
    Wheel { delta: i32, horizontal: bool },
    Key { key_code: u32, down: bool },           // platform virtual-key code
}
```

- Pointer coordinates are normalized inside the shared **screen**; letterbox areas are ignored.
  Window shares are view-only and are denied by the host.
- The host advertises `screen-control` only when it has an input backend.
- The host owns the lease (30 minutes by default, configurable 5–60). The grant carries the lease;
  both sides count down. Heartbeats every 2 s detect a dead controller; closing the channel ends
  the session.
- Input from anyone other than the active controller is ignored, as are frames outside `ACTIVE`.
  Revoking or ending a session releases every injected key and mouse button.
- The Windows host filters `Alt+F4`; other system shortcuts, including the Windows key, reach the
  presenter. `Ctrl+Alt+Del` cannot be injected.
- Input injection uses `SendInput` and is blocked by UIPI for elevated windows. macOS and Linux
  hosts do not advertise the capability in v2.

---

## 8. Errors

```json
{ "code": "E_AUTH_FAILED", "message": "invalid password", "retryable": false, "fatal": true }
```

| Code | Fatal | Meaning |
|---|---|---|
| `E_PROTO_VERSION` | yes | server does not speak this protocol version |
| `E_BAD_PAYLOAD` | no | malformed or oversized field |
| `E_AUTH_REQUIRED` | yes | message sent before `welcome` |
| `E_AUTH_FAILED` | yes | bad proof |
| `E_AUTH_RATE_LIMIT` | yes | too many attempts from this IP |
| `E_NAME_INVALID` | yes | display name rejected |
| `E_ROOM_NOT_FOUND` | no | unknown `roomId` |
| `E_ROOM_DELETE_DENIED` | no | lobby or config room cannot be removed |
| `E_ROOM_NOT_EMPTY` | no | a room with members cannot be removed |
| `E_ROOM_FULL` | no | voice room at capacity |
| `E_NOT_IN_ROOM` | no | message or signal from outside a room |
| `E_BAD_TARGET` | no | `to` is unknown, offline or outside the room |
| `E_RATE_LIMIT` | no | per-session rate limit hit |
| `E_PAYLOAD_TOO_LARGE` | no | frame or field over the limit |
| `E_INTERNAL` | no | server bug; logged with a correlation id |
| `E_SHUTTING_DOWN` | yes | server restarting |

---

## 9. Limits and rate limits

| Limit | Value |
|---|---|
| Frame size | 64 KiB |
| Chat text | 4096 UTF-8 bytes |
| Signaling payload | 32 KiB |
| Display name | 32 chars |
| Room topic | 128 chars |
| Rooms per server | 32 (client-created; configurable) |
| Members per voice room | 8 (configurable) |
| Chat rate | 5 msg/s, burst 10 |
| Signaling rate | 50 msg/s sustained, 300 burst |
| File offers | 20/min |
| Auth attempts | 3/min per IP, then 30 s lockout per failure |
| Heartbeat | server `ping` every 20 s; drop after 45 s without traffic |

Violations get the corresponding error; repeated violations (`> 3×`) close the connection with
`E_RATE_LIMIT`.

---

## 10. Reconnection

- 0.5 s → 30 s exponential backoff, ±20% jitter, indefinite.
- A reconnect is a **fresh session**: new `sessionId`, re-auth, rejoin the last room, full roster
  resync, new peer connections.
- In-flight file transfers are cancelled, not resumed.
- The server keeps no history. The client shows a "disconnected" gap marker in local history and
  resumes from there.
- Clients send `ping` immediately after `welcome` to validate the path end to end.

---

## 11. Compatibility and versioning

- `v` changes only for breaking changes. Additive fields do not bump it.
- The server advertises its supported version in the `E_PROTO_VERSION` message, and clients show
  "server is too old/new" instead of retrying.
- v1 clients are not supported. The Kotlin `PROTOCOL.md` remains the behavioural reference for
  migration and for the feature-parity checklist in `docs/PLAN.md`.
